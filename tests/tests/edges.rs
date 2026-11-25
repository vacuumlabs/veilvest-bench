//! Edge cases of the public path: block time stepping back, the limits of a cancel's split,
//! destinations the transaction leaves out, separate escrows, and values out of range.

use lee_core::account::AccountId;
use vesting_core::{
    error, event,
    seeds::{escrow_id, schedule_id},
    Message, Shape, MAX_CANCEL_LEAD, MAX_TRANCHES,
};
use vesting_tests::*;

struct Two {
    chain: Chain,
    people: People,
    escrow: AccountId,
    /// Bob's schedule, and carol's.
    bob: AccountId,
    carol: AccountId,
}

/// One batch, one escrow: 1,000 for bob and 1,000 for carol.
fn two() -> Two {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    chain.open(&people.bob, id(&people.definition));
    chain.open(&people.carol, id(&people.definition));
    chain
        .create(
            &people,
            0,
            &[
                spec(id(&people.bob), 1_000, linear()),
                spec(id(&people.carol), 1_000, linear()),
            ],
        )
        .expect("created");
    let creator = id(&people.creator);
    Two {
        escrow: escrow_id(VESTING, creator, 0),
        bob: schedule_id(VESTING, creator, 0, 0),
        carol: schedule_id(VESTING, creator, 0, 1),
        chain,
        people,
    }
}

/// Block time steps back after a claim. The cancel must not refund what was already claimed,
/// because the difference would come out of the other schedule of the batch.
#[test]
fn a_cancel_after_time_steps_back_never_takes_from_another_schedule() {
    let mut t = two();
    t.chain.now = START + 900_000;
    let now = t.chain.now;
    t.chain
        .claim(&t.people.bob, t.bob, t.escrow, 900, now, id(&t.people.bob))
        .expect("claimed");
    t.chain.now = START + 100_000;
    let until = t.chain.now + 1;
    t.chain
        .cancel(
            &t.people.creator,
            t.bob,
            t.escrow,
            id(&t.people.source),
            until,
        )
        .expect("cancelled");
    assert_eq!(
        t.chain.balance(t.escrow),
        1_000,
        "carol's tokens are untouched"
    );
    t.chain.now = END;
    t.chain
        .claim(
            &t.people.carol,
            t.carol,
            t.escrow,
            1_000,
            END,
            id(&t.people.carol),
        )
        .expect("carol is paid in full");
}

/// Asynchronous credits to an account the transaction leaves out would stay pending. Here the
/// transaction is refused instead, so nothing is recorded as paid.
#[test]
fn a_claim_that_leaves_its_destination_out_is_refused() {
    let mut t = two();
    t.chain.now = END;
    let claim = Message::Claim {
        schedule: t.bob,
        amount: 400,
        as_of: END,
        to: id(&t.people.bob),
        open_holding: None,
    };
    let result = t.chain.act(&t.people.bob, t.bob, &[t.escrow], &claim);
    assert_refused(&result, "UnboundAsyncDestination");
    assert_eq!(t.chain.schedule(t.bob).unwrap().claimed, 0);
    assert_eq!(t.chain.balance(t.escrow), 2_000);
}

#[test]
fn a_cancel_that_leaves_the_refund_holding_out_is_refused() {
    let mut t = two();
    t.chain.now = START + 500_000;
    let until = t.chain.now + 1;
    let cancel = Message::Cancel {
        schedule: t.bob,
        until,
    };
    let result = t.chain.act(&t.people.creator, t.bob, &[t.escrow], &cancel);
    assert_refused(&result, "UnboundAsyncDestination");
    assert!(t.chain.schedule(t.bob).unwrap().cancelled_at.is_none());
    assert_eq!(t.chain.balance(t.escrow), 2_000);
}

#[test]
fn a_cancelled_schedule_cannot_be_made_non_cancelable() {
    let mut t = two();
    let until = t.chain.now + 1;
    t.chain
        .cancel(
            &t.people.creator,
            t.bob,
            t.escrow,
            id(&t.people.source),
            until,
        )
        .expect("cancelled");
    let message = Message::MakeNonCancelable { schedule: t.bob };
    let result = t.chain.act(&t.people.creator, t.bob, &[], &message);
    assert_refused(&result, error::ALREADY_CANCELLED);
}

/// A split stated far ahead would leave the beneficiary everything and use up the cancel, so a
/// delegated cancel authority could void the creator's refund.
#[test]
fn a_cancel_cannot_split_further_ahead_than_the_limit() {
    assert_eq!(MAX_CANCEL_LEAD, 60 * 60 * 1_000, "one hour");
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let shape = Shape::Linear {
        start: START,
        end: START + 10 * MAX_CANCEL_LEAD,
    };
    chain
        .create(&people, 0, &[spec(id(&people.bob), 1_000, shape)])
        .expect("created");
    let creator = id(&people.creator);
    let escrow = escrow_id(VESTING, creator, 0);
    let schedule = schedule_id(VESTING, creator, 0, 0);
    chain.now = START + MAX_CANCEL_LEAD;
    let latest = chain.now + MAX_CANCEL_LEAD;
    for until in [latest + 1, START + 10 * MAX_CANCEL_LEAD, u64::MAX] {
        assert_refused(
            &chain.cancel(&people.creator, schedule, escrow, id(&people.source), until),
            "OutOfValidityWindow",
        );
    }
    assert!(chain.schedule(schedule).unwrap().cancelled_at.is_none());
    chain
        .cancel(
            &people.creator,
            schedule,
            escrow,
            id(&people.source),
            latest,
        )
        .expect("the limit itself is allowed");
    assert_eq!(
        chain.balance(escrow),
        200,
        "a tenth per limit, two limits in"
    );
}

/// The window of a claim covers the cliff too, not only linear schedules.
#[test]
fn the_lump_of_a_cliff_cannot_be_claimed_early() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    chain.open(&people.bob, id(&people.definition));
    let cliff = START + 500_000;
    let shape = Shape::CliffLinear {
        start: START,
        cliff,
        end: END,
    };
    let bob = id(&people.bob);
    chain
        .create(&people, 0, &[spec(bob, 1_000, shape)])
        .expect("created");
    let creator = id(&people.creator);
    let escrow = escrow_id(VESTING, creator, 0);
    let schedule = schedule_id(VESTING, creator, 0, 0);
    chain.now = cliff - 1;
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 500, cliff, bob),
        "OutOfValidityWindow",
    );
    chain.now = cliff;
    chain
        .claim(&people.bob, schedule, escrow, 500, cliff, bob)
        .expect("at the cliff");
}

/// The lead need not reach past a cliff: a split just before it returns the whole lump.
#[test]
fn a_cancel_before_the_cliff_can_stop_short_of_it() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let cliff = START + 500_000;
    let shape = Shape::CliffLinear {
        start: START,
        cliff,
        end: END,
    };
    chain
        .create(&people, 0, &[spec(id(&people.bob), 1_000, shape)])
        .expect("created");
    let creator = id(&people.creator);
    let escrow = escrow_id(VESTING, creator, 0);
    let schedule = schedule_id(VESTING, creator, 0, 0);
    chain.now = cliff - 60_000;
    chain
        .cancel(
            &people.creator,
            schedule,
            escrow,
            id(&people.source),
            cliff - 1,
        )
        .expect("cancelled");
    assert_eq!(chain.balance(escrow), 0);
    assert_eq!(chain.balance(id(&people.source)), 10_000);
}

/// Each batch of each creator has its own escrow, and a schedule pays only out of its own.
#[test]
fn a_schedule_pays_only_from_the_escrow_of_its_creator_and_batch() {
    let first = people();
    let second = People {
        creator: key(20),
        source: key(21),
        ..people()
    };
    let definition = id(&first.definition);
    let mut chain = Chain::funded(&first, 10_000);
    chain.open(&first.bob, definition);
    chain.open(&second.source, definition);
    chain
        .submit(
            token(id(&first.source)),
            &[token(id(&first.source)), token(id(&second.source))],
            &[&first.source],
            &token_core::Message::Transfer {
                to: id(&second.source),
                amount: 5_000,
            },
        )
        .expect("funds the second creator");
    let batches = [(&first, 0), (&first, 1), (&second, 0), (&second, 1)];
    for (people, batch) in batches {
        chain
            .create(people, batch, &[spec(id(&first.bob), 1_000, linear())])
            .expect("created");
    }
    chain.now = END;
    for (done, (people, batch)) in batches.into_iter().enumerate() {
        let creator = id(&people.creator);
        chain
            .claim(
                &first.bob,
                schedule_id(VESTING, creator, batch, 0),
                escrow_id(VESTING, creator, batch),
                1_000,
                END,
                id(&first.bob),
            )
            .expect("claimed");
        for (other, (people, batch)) in batches.into_iter().enumerate() {
            assert_eq!(
                chain.balance(escrow_id(VESTING, id(&people.creator), batch)),
                if other <= done { 0 } else { 1_000 }
            );
        }
    }
}

#[test]
fn a_delegated_cancel_authority_cannot_convert() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let mut spec = spec(id(&people.bob), 1_000, linear());
    spec.cancel_authority = Some(id(&people.carol));
    chain.create(&people, 0, &[spec]).expect("created");
    let schedule = schedule_id(VESTING, id(&people.creator), 0, 0);
    let convert = Message::MakeNonCancelable { schedule };
    assert_refused(
        &chain.act(&people.carol, schedule, &[], &convert),
        error::NOT_CREATOR,
    );
    let events = chain
        .act(&people.creator, schedule, &[], &convert)
        .expect("the creator converts");
    assert_eq!(
        events_of::<event::MadeNonCancelable>(&events),
        vec![event::MadeNonCancelable { schedule }]
    );
}

#[test]
fn a_transfer_announces_the_old_and_the_new_beneficiary() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let (old, new) = (id(&people.bob), id(&people.carol));
    let mut spec = spec(old, 1_000, linear());
    spec.transferable = true;
    chain.create(&people, 0, &[spec]).expect("created");
    let schedule = schedule_id(VESTING, id(&people.creator), 0, 0);
    let transfer = Message::TransferBeneficiary { schedule, to: new };
    let events = chain
        .act(&people.bob, schedule, &[], &transfer)
        .expect("transferred");
    assert_eq!(
        events_of::<event::BeneficiaryTransferred>(&events),
        vec![event::BeneficiaryTransferred { schedule, old, new }]
    );
}

/// Sums that wrap around 128 bits, and more tranches than there are bits.
#[test]
fn totals_and_tranches_out_of_range_are_refused() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let bob = id(&people.bob);
    let milestones = |tranches: Vec<u128>, total| {
        let shape = Shape::Milestones {
            tranches,
            signalled: 0,
        };
        spec(bob, total, shape)
    };
    for batch in [
        vec![spec(bob, u128::MAX, linear()), spec(bob, 11, linear())],
        vec![milestones(vec![u128::MAX, 11], 10)],
        vec![milestones(vec![1; MAX_TRANCHES + 1], 65)],
        vec![milestones(vec![10, 0], 10)],
        vec![milestones(vec![], 10)],
    ] {
        assert_refused(&chain.create(&people, 0, &batch), error::BAD_SCHEDULE);
    }
    chain
        .create(&people, 0, &[milestones(vec![1; MAX_TRANCHES], 64)])
        .expect("the limit itself is allowed");
}

/// Time plays no part in a milestone schedule, so its claims have no window.
#[test]
fn a_milestone_claim_ignores_its_time() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    chain.open(&people.bob, id(&people.definition));
    let shape = Shape::Milestones {
        tranches: vec![400, 600],
        signalled: 0,
    };
    let bob = id(&people.bob);
    chain
        .create(&people, 0, &[spec(bob, 1_000, shape)])
        .expect("created");
    let creator = id(&people.creator);
    let escrow = escrow_id(VESTING, creator, 0);
    let schedule = schedule_id(VESTING, creator, 0, 0);
    let signal = Message::SignalMilestone { schedule, index: 1 };
    chain
        .act(&people.creator, schedule, &[], &signal)
        .expect("signalled");
    chain
        .claim(&people.bob, schedule, escrow, 600, u64::MAX, bob)
        .expect("a far-off time does not matter");
    assert_refused(
        &chain.claim(&people.bob, schedule, escrow, 1, u64::MAX, bob),
        error::EXCEEDS_VESTED,
    );
}
