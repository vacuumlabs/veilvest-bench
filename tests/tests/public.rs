//! The public lifecycle: the real vesting and token guests, through LEZ's public execution.

use lee_core::account::AccountId;
use proptest::prelude::*;
use vesting_core::{
    error, event,
    seeds::{escrow_id, schedule_id},
    Message, ScheduleSpec, Shape,
};
use vesting_tests::*;

fn one(people: &People) -> (AccountId, AccountId) {
    let creator = id(&people.creator);
    (
        escrow_id(VESTING, creator, 0),
        schedule_id(VESTING, creator, 0, 0),
    )
}

/// A funded chain with one schedule for bob, and bob's holding open.
fn with_schedule(spec: ScheduleSpec) -> (Chain, People, AccountId, AccountId) {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    chain.open(&people.bob, id(&people.definition));
    chain.create(&people, 0, &[spec]).expect("created");
    let (escrow, schedule) = one(&people);
    (chain, people, escrow, schedule)
}

fn bob() -> AccountId {
    id(&people().bob)
}

#[test]
fn a_batch_locks_its_total_and_announces_each_schedule() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let specs = [
        spec(id(&people.bob), 1_000, linear()),
        spec(id(&people.carol), 500, linear()),
    ];
    let events = chain.create(&people, 0, &specs).expect("created");
    let creator = id(&people.creator);
    assert_eq!(chain.balance(escrow_id(VESTING, creator, 0)), 1_500);
    assert_eq!(chain.balance(id(&people.source)), 8_500);
    let announced = |batch| {
        (0u32..)
            .zip(&specs)
            .map(move |(index, spec)| event::Created {
                schedule: schedule_id(VESTING, creator, batch, index),
                creator,
                batch,
                spec: spec.clone(),
            })
    };
    assert!(events_of::<event::Created>(&events)
        .into_iter()
        .eq(announced(0)));
    // The next batch has its own number and its own escrow.
    let events = chain.create(&people, 1, &specs).expect("created again");
    assert_eq!(chain.balance(escrow_id(VESTING, creator, 1)), 1_500);
    assert!(events_of::<event::Created>(&events)
        .into_iter()
        .eq(announced(1)));
}

#[test]
fn a_create_that_cannot_fund_its_escrow_leaves_nothing() {
    let people = people();
    let mut chain = Chain::funded(&people, 100);
    let specs = [spec(id(&people.bob), 101, linear())];
    assert_refused(&chain.create(&people, 0, &specs), "insufficient balance");
    let (_, schedule) = one(&people);
    assert_eq!(chain.schedule(schedule), None);
    assert_eq!(chain.balance(id(&people.source)), 100);
}

/// The credit to the escrow is asynchronous. Left undeclared it would stay pending, so the escrow
/// must not count as funded.
#[test]
fn a_create_that_leaves_the_escrow_out_is_refused() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let specs = [spec(id(&people.bob), 1_000, linear())];
    let (escrow, schedule) = one(&people);
    let (mut declared, message) = chain.create_parts(&people, 0, &specs);
    declared.retain(|actor| *actor != token(escrow));
    let result = chain.submit(
        vesting(id(&people.creator)),
        &declared,
        &[&people.creator, &people.source],
        &message,
    );
    assert_refused(&result, "UndeclaredActor");
    assert_eq!(chain.schedule(schedule), None);
    assert_eq!(chain.balance(id(&people.source)), 10_000);
}

#[test]
fn a_create_funded_with_another_token_is_refused() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    // `other` holds a worthless token; the schedule names the real one.
    let worthless = key(20);
    chain.define(&worthless, &people.other, 10_000);
    let specs = [spec(id(&people.bob), 1_000, linear())];
    let (escrow, schedule) = one(&people);
    let (mut declared, _) = chain.create_parts(&people, 0, &specs);
    declared.push(token(id(&people.other)));
    let result = chain.submit(
        vesting(id(&people.creator)),
        &declared,
        &[&people.creator, &people.other],
        &Message::CreateBatch {
            token: id(&people.definition),
            source: id(&people.other),
            schedules: specs.to_vec(),
        },
    );
    assert_refused(&result, "another token");
    assert_eq!(chain.schedule(schedule), None);
    assert_eq!(chain.balance(escrow), 0);
}

#[test]
fn the_funding_holding_must_sign() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let specs = [spec(id(&people.bob), 1_000, linear())];
    let (declared, message) = chain.create_parts(&people, 0, &specs);
    let result = chain.submit(
        vesting(id(&people.creator)),
        &declared,
        &[&people.creator],
        &message,
    );
    assert_refused(&result, "must authorize");
}

#[test]
fn a_claim_pays_exactly_its_amount_and_repeats_up_to_what_vested() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = START + 400_000;
    let events = chain
        .claim(&people.bob, schedule, escrow, 250, chain.now, bob())
        .expect("claimed");
    assert_eq!(chain.balance(bob()), 250);
    assert_eq!(
        events_of::<event::Claimed>(&events),
        vec![event::Claimed {
            schedule,
            beneficiary: bob(),
            amount: 250,
            as_of: chain.now,
            claimed: 250,
        }]
    );
    let now = chain.now;
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 151, now, bob()),
        error::EXCEEDS_VESTED,
    );
    chain
        .claim(&people.bob, schedule, escrow, 150, now, bob())
        .expect("the rest of what vested");
    chain.now = END;
    chain
        .claim(&people.bob, schedule, escrow, 600, END, bob())
        .expect("the remainder");
    assert_eq!((chain.balance(bob()), chain.balance(escrow)), (1_000, 0));
}

#[test]
fn a_claim_for_the_future_is_refused_and_an_old_one_stays_valid() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = START + 100_000;
    let future = chain.now + 1;
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 100, future, bob()),
        "OutOfValidityWindow",
    );
    assert_eq!(chain.schedule(schedule).unwrap().claimed, 0);
    let as_of = chain.now;
    chain.now = END + 1_000_000;
    chain
        .claim(&people.bob, schedule, escrow, 100, as_of, bob())
        .expect("a claim computed long ago");
}

#[test]
fn only_the_beneficiary_claims() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = END;
    for thief in [&people.creator, &people.other] {
        assert_refused(
            &chain.claim(thief, schedule, escrow, 1, END, id(thief)),
            error::NOT_BENEFICIARY,
        );
    }
    // Straight at the schedule, or at bob's actor without bob's signature.
    let claim = Message::Claim {
        schedule,
        amount: 1,
        as_of: END,
        to: id(&people.other),
        open_holding: None,
    };
    let declared = act_declared(bob(), schedule, &[escrow, id(&people.other)]);
    assert_refused(
        &chain.submit(vesting(bob()), &declared, &[&people.other], &claim),
        error::NOT_AUTHORIZED,
    );
    let direct = vesting_core::Internal::Claim {
        amount: 1,
        as_of: END,
        to: id(&people.other),
    };
    assert_refused(
        &chain.submit(vesting(schedule), &declared, &[&people.bob], &direct),
        error::UNEXPECTED,
    );
    assert_eq!(chain.balance(escrow), 1_000);
}

#[test]
fn a_claim_whose_payout_fails_consumes_nothing() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = END;
    // The destination cannot hold tokens, so the credit fails.
    let to = id(&people.definition);
    let result = chain.claim(&people.bob, schedule, escrow, 10, END, to);
    assert_refused(&result, "recipient is a token definition");
    assert_eq!(chain.schedule(schedule).unwrap().claimed, 0);
    assert_eq!(chain.balance(escrow), 1_000);
    chain
        .claim(&people.bob, schedule, escrow, 10, END, bob())
        .expect("the retry succeeds");
}

#[test]
fn a_claim_can_open_its_destination_when_the_destination_signs() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = END;
    let fresh = key(40);
    let mut declared = act_declared(bob(), schedule, &[escrow, id(&fresh)]);
    declared.push(token(id(&people.definition)));
    let claim = Message::Claim {
        schedule,
        amount: 10,
        as_of: END,
        to: id(&fresh),
        open_holding: Some(id(&people.definition)),
    };
    chain
        .submit(vesting(bob()), &declared, &[&people.bob, &fresh], &claim)
        .expect("claimed into a new holding");
    assert_eq!(chain.balance(id(&fresh)), 10);
}

#[test]
fn cancel_splits_at_its_stated_time_and_refunds_the_funding_holding() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = START + 300_000;
    let until = chain.now + 50_000;
    let events = chain
        .cancel(&people.creator, schedule, escrow, id(&people.source), until)
        .expect("cancelled");
    assert_eq!(
        events_of::<event::Cancelled>(&events),
        vec![event::Cancelled {
            schedule,
            by: id(&people.creator),
            until,
            vested: 350,
            refunded: 650,
        }]
    );
    assert_eq!(chain.balance(id(&people.source)), 9_650);
    // Vested but unclaimed stays claimable, and time no longer adds to it.
    chain.now = END;
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 351, END, bob()),
        error::EXCEEDS_VESTED,
    );
    chain
        .claim(&people.bob, schedule, escrow, 350, END, bob())
        .expect("the vested part");
    assert_eq!(chain.balance(escrow), 0);
    assert_refused(
        &chain.cancel(
            &people.creator,
            schedule,
            escrow,
            id(&people.source),
            END + 1,
        ),
        error::ALREADY_CANCELLED,
    );
}

#[test]
fn a_backdated_cancel_is_refused() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    chain.now = START + 300_000;
    for until in [START, START + 299_999, START + 300_000] {
        assert_refused(
            &chain.cancel(&people.creator, schedule, escrow, id(&people.source), until),
            "OutOfValidityWindow",
        );
    }
    assert_eq!(chain.balance(escrow), 1_000);
}

#[test]
fn make_non_cancelable_is_final() {
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    let convert = Message::MakeNonCancelable { schedule };
    assert_refused(
        &chain.act(&people.other, schedule, &[], &convert),
        error::NOT_CREATOR,
    );
    chain
        .act(&people.creator, schedule, &[], &convert)
        .expect("converted");
    assert_refused(
        &chain.cancel(&people.creator, schedule, escrow, id(&people.source), END),
        error::NOT_CANCELABLE,
    );
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &convert),
        error::NOT_CANCELABLE,
    );
}

#[test]
fn milestones_unlock_once_each_and_survive_a_cancel() {
    let shape = Shape::Milestones {
        tranches: vec![100, 300, 600],
        signalled: 0,
    };
    let (mut chain, people, escrow, schedule) = with_schedule(spec(bob(), 1_000, shape));
    let signal = |index| Message::SignalMilestone { schedule, index };
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 1, u64::MAX, bob()),
        error::EXCEEDS_VESTED,
    );
    assert_refused(
        &chain.act(&people.bob, schedule, &[], &signal(1)),
        error::NOT_MILESTONE_AUTHORITY,
    );
    let events = chain
        .act(&people.creator, schedule, &[], &signal(1))
        .expect("signalled");
    assert_eq!(
        events_of::<event::MilestoneSignalled>(&events),
        vec![event::MilestoneSignalled {
            schedule,
            index: 1,
            amount: 300,
        }]
    );
    let before = chain.schedule(schedule);
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &signal(1)),
        error::ALREADY_SIGNALLED,
    );
    assert_eq!(chain.schedule(schedule), before);
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &signal(3)),
        error::UNKNOWN_MILESTONE,
    );
    let until = chain.now + 1;
    chain
        .cancel(&people.creator, schedule, escrow, id(&people.source), until)
        .expect("cancelled");
    assert_eq!(chain.balance(id(&people.source)), 9_700);
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &signal(0)),
        error::ALREADY_CANCELLED,
    );
    chain
        .claim(&people.bob, schedule, escrow, 300, 0, bob())
        .expect("the signalled tranche");
    assert_eq!(chain.balance(escrow), 0);
}

#[test]
fn delegated_roles_exclude_the_creator() {
    let people = people();
    let mut spec = spec(
        bob(),
        1_000,
        Shape::Milestones {
            tranches: vec![1_000],
            signalled: 0,
        },
    );
    spec.cancel_authority = Some(id(&people.carol));
    spec.milestone_authority = Some(id(&people.other));
    let (mut chain, people, escrow, schedule) = with_schedule(spec);
    let until = chain.now + 1;
    let signal = Message::SignalMilestone { schedule, index: 0 };
    assert_refused(
        &chain.cancel(&people.creator, schedule, escrow, id(&people.source), until),
        error::NOT_CANCEL_AUTHORITY,
    );
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &signal),
        error::NOT_MILESTONE_AUTHORITY,
    );
    chain
        .act(&people.other, schedule, &[], &signal)
        .expect("the milestone authority signals");
    chain
        .cancel(&people.carol, schedule, escrow, id(&people.source), until)
        .expect("the cancel authority cancels");
}

#[test]
fn a_position_moves_only_if_created_transferable() {
    let to = id(&people().carol);
    let transfer = |schedule| Message::TransferBeneficiary { schedule, to };
    let (mut chain, people, _, schedule) = with_schedule(spec(bob(), 1_000, linear()));
    assert_refused(
        &chain.act(&people.bob, schedule, &[], &transfer(schedule)),
        error::NOT_TRANSFERABLE,
    );

    let mut movable = spec(bob(), 1_000, linear());
    movable.transferable = true;
    let (mut chain, people, escrow, schedule) = with_schedule(movable);
    assert_refused(
        &chain.act(&people.creator, schedule, &[], &transfer(schedule)),
        error::NOT_BENEFICIARY,
    );
    chain
        .act(&people.bob, schedule, &[], &transfer(schedule))
        .expect("transferred");
    chain.open(&people.carol, id(&people.definition));
    chain.now = END;
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 1, END, bob()),
        error::NOT_BENEFICIARY,
    );
    chain
        .claim(&people.carol, schedule, escrow, 1_000, END, to)
        .expect("the new beneficiary claims");
}

#[test]
fn malformed_schedules_are_refused() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let bad = [
        spec(bob(), 0, linear()),
        spec(
            bob(),
            10,
            Shape::Linear {
                start: END,
                end: START,
            },
        ),
        spec(
            bob(),
            10,
            Shape::Milestones {
                tranches: vec![9],
                signalled: 0,
            },
        ),
    ];
    for spec in bad {
        assert_refused(&chain.create(&people, 0, &[spec]), error::BAD_SCHEDULE);
    }
    assert_refused(&chain.create(&people, 0, &[]), error::BAD_SCHEDULE);
}

#[derive(Debug, Clone)]
enum Step {
    Wait(u64),
    /// Block time is the sequencer's clock, which a correction can move back.
    StepBack(u64),
    Claim {
        who: bool,
        part: u8,
    },
    Cancel {
        who: bool,
        ahead: u64,
    },
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![
            (0u64..400_000).prop_map(Step::Wait),
            (0u64..400_000).prop_map(Step::StepBack),
            (any::<bool>(), any::<u8>()).prop_map(|(who, part)| Step::Claim { who, part }),
            (any::<bool>(), 1u64..200_000).prop_map(|(who, ahead)| Step::Cancel { who, ahead }),
        ],
        8..16,
    )
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Two schedules on one escrow, random operations in random order, block time moving both
    /// ways from half way through: the escrow always holds exactly what is neither claimed nor
    /// refunded, and a cancel never lowers what its beneficiary could claim when it landed.
    #[test]
    fn the_escrow_covers_its_schedules_exactly(steps in steps()) {
        let people = people();
        let mut chain = Chain::funded(&people, 10_000);
        chain.open(&people.bob, id(&people.definition));
        chain.open(&people.carol, id(&people.definition));
        let cliff = Shape::CliffLinear { start: START, cliff: START + 333_333, end: END };
        chain.create(&people, 0, &[
            spec(id(&people.bob), 1_009, linear()),
            spec(id(&people.carol), 2_003, cliff),
        ]).expect("created");
        chain.now = START + 500_000;
        let creator = id(&people.creator);
        let escrow = escrow_id(VESTING, creator, 0);
        let ids = [schedule_id(VESTING, creator, 0, 0), schedule_id(VESTING, creator, 0, 1)];
        let people_of = [&people.bob, &people.carol];
        let mut refunded = [0u128; 2];
        for step in steps {
            match step {
                Step::Wait(ms) => chain.now += ms,
                Step::StepBack(ms) => chain.now = chain.now.saturating_sub(ms),
                Step::Claim { who, part } => {
                    let i = usize::from(who);
                    let open = chain.schedule(ids[i]).unwrap().claimable(chain.now);
                    let amount = open * u128::from(part) / 255;
                    let now = chain.now;
                    let result = chain.claim(people_of[i], ids[i], escrow, amount, now, id(people_of[i]));
                    prop_assert_eq!(result.is_ok(), amount > 0);
                }
                Step::Cancel { who, ahead } => {
                    let i = usize::from(who);
                    let before = chain.schedule(ids[i]).unwrap();
                    let until = chain.now + ahead;
                    let result = chain.cancel(&people.creator, ids[i], escrow, id(&people.source), until);
                    prop_assert_eq!(result.is_ok(), before.cancelled_at.is_none());
                    if result.is_ok() {
                        refunded[i] = before.total - before.vested(until).max(before.claimed);
                        let after = chain.schedule(ids[i]).unwrap();
                        prop_assert!(after.claimable(chain.now) >= before.claimable(chain.now));
                        prop_assert_eq!(after.vested(u64::MAX).max(after.claimed) + refunded[i], before.total);
                    }
                }
            }
            let owed: u128 = (0..2).map(|i| {
                let s = chain.schedule(ids[i]).unwrap();
                s.total - s.claimed - refunded[i]
            }).sum();
            prop_assert_eq!(chain.balance(escrow), owed);
        }
    }
}
