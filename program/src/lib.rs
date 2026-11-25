//! The vesting program on LEZ's actor interface.
//!
//! A person acts through the vesting actor on their own account: it checks that the account
//! authorised the message and forwards it to the schedule actor, which accepts only messages from
//! this program and compares the sender's account with the stored role. Nothing reads a clock:
//! a claim states the time it is computed for and a cancel the time it splits at, at most an hour
//! ahead, and the transaction's validity window ties both to the block's timestamp.

use lee_core::{
    account::{AccountId, Actor},
    program::{ReceiveInput, Response, SyncMessage},
};
pub use vesting_core as core;
use vesting_core::{
    error,
    event::{
        BeneficiaryTransferred, Cancelled, Claimed, Created, MadeNonCancelable, MilestoneSignalled,
        VestingEvent as _,
    },
    seeds, CreatorState, Internal, Message, Schedule, ScheduleSpec, Shape, MAX_CANCEL_LEAD,
    TOKEN_PROGRAM,
};

pub fn handle_message(input: &ReceiveInput) -> Response {
    if input.from_own_program() {
        let message = borsh::from_slice(&input.message).expect(error::UNEXPECTED);
        return on_schedule(input, message);
    }
    let message = borsh::from_slice(&input.message).expect(error::UNEXPECTED);
    on_person(input, message)
}

// ---- at a person's own vesting actor ----

fn on_person(input: &ReceiveInput, message: Message) -> Response {
    assert!(input.is_authorized, "{}", error::NOT_AUTHORIZED);
    match message {
        Message::CreateBatch {
            token,
            source,
            schedules,
        } => create_batch(input, token, source, schedules),
        Message::Claim {
            schedule,
            amount,
            as_of,
            to,
            open_holding,
        } => {
            let open = open_holding.map(|definition| {
                SyncMessage::new(
                    token(to),
                    &token_core::Message::InitializeHolding { definition },
                )
            });
            let response = open
                .into_iter()
                .fold(Response::keep_state(), Response::send);
            response.send_sync(own(input, schedule), &Internal::Claim { amount, as_of, to })
        }
        Message::Cancel { schedule, until } => {
            forward(input, schedule, &Internal::Cancel { until })
        }
        Message::MakeNonCancelable { schedule } => {
            forward(input, schedule, &Internal::MakeNonCancelable)
        }
        Message::SignalMilestone { schedule, index } => {
            forward(input, schedule, &Internal::SignalMilestone { index })
        }
        Message::TransferBeneficiary { schedule, to } => {
            forward(input, schedule, &Internal::TransferBeneficiary { to })
        }
    }
}

fn forward(input: &ReceiveInput, schedule: AccountId, message: &Internal) -> Response {
    Response::keep_state().send_sync(own(input, schedule), message)
}

/// Opens the escrow, funds it, then writes the schedules, all in one transaction. Opening the
/// escrow makes the transaction declare it, and an async credit to a declared public actor runs at
/// once, so the funding cannot stay pending; a holding of another token refuses it.
fn create_batch(
    input: &ReceiveInput,
    token_definition: AccountId,
    source: AccountId,
    schedules: Vec<ScheduleSpec>,
) -> Response {
    let mut state = creator_state(input);
    assert!(!schedules.is_empty(), "{}", error::BAD_SCHEDULE);
    let creator = input.receiver.account_id;
    let batch = state.next_batch;
    let total = schedules
        .iter()
        .try_fold(0u128, |sum, spec| {
            assert!(
                spec.total > 0 && spec.shape.is_valid(spec.total),
                "{}",
                error::BAD_SCHEDULE
            );
            sum.checked_add(spec.total)
        })
        .expect(error::BAD_SCHEDULE);
    let seed = seeds::escrow(creator, batch);
    let escrow = AccountId::for_public_pda(&input.receiver.program_account_id, &seed);
    state.next_batch += 1;

    let response = Response::set_state(encode(&state))
        .send(
            SyncMessage::new(
                token(escrow),
                &token_core::Message::InitializeHolding {
                    definition: token_definition,
                },
            )
            .with_pda_seeds([seed]),
        )
        .send_sync(
            token(source),
            &token_core::Message::Transfer {
                to: escrow,
                amount: total,
            },
        );
    schedules
        .into_iter()
        .enumerate()
        .fold(response, |response, (index, spec)| {
            let index = u32::try_from(index).expect(error::BAD_SCHEDULE);
            let seed = seeds::schedule(creator, batch, index);
            let id = AccountId::for_public_pda(&input.receiver.program_account_id, &seed);
            response.send(
                SyncMessage::new(
                    own(input, id),
                    &Internal::Init {
                        creator,
                        batch,
                        source,
                        spec,
                    },
                )
                .with_pda_seeds([seed]),
            )
        })
}

fn creator_state(input: &ReceiveInput) -> CreatorState {
    if input.pre_state.is_empty() {
        return CreatorState::default();
    }
    borsh::from_slice(&input.pre_state).expect(error::UNEXPECTED)
}

// ---- at a schedule ----

fn on_schedule(input: &ReceiveInput, message: Internal) -> Response {
    let sender = input
        .from
        .expect("an internal message has a sender")
        .account_id;
    if let Internal::Init {
        creator,
        batch,
        source,
        spec,
    } = message
    {
        // A private account may act under another address; the escrow derives from its own.
        assert!(sender == creator, "{}", error::NOT_CREATOR);
        return init(input, sender, batch, source, spec);
    }
    let mut schedule: Schedule = borsh::from_slice(&input.pre_state).expect(error::UNEXPECTED);
    match message {
        Internal::Init { .. } => unreachable!(),
        Internal::Claim { amount, as_of, to } => {
            assert!(sender == schedule.beneficiary, "{}", error::NOT_BENEFICIARY);
            assert!(amount > 0, "{}", error::ZERO_AMOUNT);
            assert!(
                amount <= schedule.claimable(as_of),
                "{}",
                error::EXCEEDS_VESTED
            );
            schedule.claimed += amount;
            let response = Response::set_state(encode(&schedule))
                .event(
                    Claimed {
                        schedule: input.receiver.account_id,
                        beneficiary: sender,
                        amount,
                        as_of,
                        claimed: schedule.claimed,
                    }
                    .to_program_event(),
                )
                .send(pay(input, &schedule, to, amount));
            if schedule.shape.is_time_based() {
                response.timestamp_window(as_of..)
            } else {
                response
            }
        }
        Internal::Cancel { until } => {
            assert!(
                sender == schedule.cancel_authority,
                "{}",
                error::NOT_CANCEL_AUTHORITY
            );
            assert!(
                schedule.cancelled_at.is_none(),
                "{}",
                error::ALREADY_CANCELLED
            );
            assert!(schedule.cancelable, "{}", error::NOT_CANCELABLE);
            // Never below what was claimed: block time may step back, and the escrow is shared.
            let vested = schedule.vested(until).max(schedule.claimed);
            let refunded = schedule.total - vested;
            schedule.cancelled_at = Some(until);
            let response = Response::set_state(encode(&schedule))
                .event(
                    Cancelled {
                        schedule: input.receiver.account_id,
                        by: sender,
                        until,
                        vested,
                        refunded,
                    }
                    .to_program_event(),
                )
                .try_timestamp_window(until.saturating_sub(MAX_CANCEL_LEAD)..until)
                .expect(error::UNEXPECTED);
            if refunded == 0 {
                return response;
            }
            response.send(pay(input, &schedule, schedule.source, refunded))
        }
        Internal::MakeNonCancelable => {
            assert!(sender == schedule.creator, "{}", error::NOT_CREATOR);
            assert!(
                schedule.cancelled_at.is_none(),
                "{}",
                error::ALREADY_CANCELLED
            );
            assert!(schedule.cancelable, "{}", error::NOT_CANCELABLE);
            schedule.cancelable = false;
            Response::set_state(encode(&schedule)).event(
                MadeNonCancelable {
                    schedule: input.receiver.account_id,
                }
                .to_program_event(),
            )
        }
        Internal::SignalMilestone { index } => {
            assert!(
                sender == schedule.milestone_authority,
                "{}",
                error::NOT_MILESTONE_AUTHORITY
            );
            assert!(
                schedule.cancelled_at.is_none(),
                "{}",
                error::ALREADY_CANCELLED
            );
            let Shape::Milestones {
                tranches,
                signalled,
            } = &mut schedule.shape
            else {
                panic!("{}", error::UNKNOWN_MILESTONE)
            };
            let amount = *tranches
                .get(usize::from(index))
                .expect(error::UNKNOWN_MILESTONE);
            let bit = 1u64 << index;
            assert!(*signalled & bit == 0, "{}", error::ALREADY_SIGNALLED);
            *signalled |= bit;
            Response::set_state(encode(&schedule)).event(
                MilestoneSignalled {
                    schedule: input.receiver.account_id,
                    index,
                    amount,
                }
                .to_program_event(),
            )
        }
        Internal::TransferBeneficiary { to } => {
            assert!(sender == schedule.beneficiary, "{}", error::NOT_BENEFICIARY);
            assert!(schedule.transferable, "{}", error::NOT_TRANSFERABLE);
            schedule.beneficiary = to;
            Response::set_state(encode(&schedule)).event(
                BeneficiaryTransferred {
                    schedule: input.receiver.account_id,
                    old: sender,
                    new: to,
                }
                .to_program_event(),
            )
        }
    }
}

fn init(
    input: &ReceiveInput,
    creator: AccountId,
    batch: u64,
    source: AccountId,
    spec: ScheduleSpec,
) -> Response {
    assert!(
        input.is_authorized && input.pre_state.is_empty(),
        "{}",
        error::UNEXPECTED
    );
    let schedule = Schedule {
        creator,
        batch,
        beneficiary: spec.beneficiary,
        source,
        total: spec.total,
        claimed: 0,
        shape: spec.shape.clone(),
        cancelable: spec.cancelable,
        transferable: spec.transferable,
        cancel_authority: spec.cancel_authority.unwrap_or(creator),
        milestone_authority: spec.milestone_authority.unwrap_or(creator),
        cancelled_at: None,
    };
    Response::set_state(encode(&schedule)).event(
        Created {
            schedule: input.receiver.account_id,
            creator,
            batch,
            spec,
        }
        .to_program_event(),
    )
}

/// A token transfer out of the schedule's escrow, authorised by the escrow's seed.
fn pay(input: &ReceiveInput, schedule: &Schedule, to: AccountId, amount: u128) -> SyncMessage {
    let seed = seeds::escrow(schedule.creator, schedule.batch);
    let escrow = AccountId::for_public_pda(&input.receiver.program_account_id, &seed);
    SyncMessage::new(token(escrow), &token_core::Message::Transfer { to, amount })
        .with_pda_seeds([seed])
}

fn token(account: AccountId) -> Actor {
    Actor::new(account, TOKEN_PROGRAM)
}

fn own(input: &ReceiveInput, account: AccountId) -> Actor {
    Actor::new(account, input.receiver.program_account_id)
}

fn encode<T: borsh::BorshSerialize>(value: &T) -> Vec<u8> {
    borsh::to_vec(value).expect("borsh serialization is infallible")
}
