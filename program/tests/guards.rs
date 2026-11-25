//! Guards that no transaction of the bench can reach. Only the program itself sends a schedule
//! its first message, with the seed and to a fresh address, and the bench has no other program
//! that could send one of these messages.

use lee_core::{
    account::{AccountId, Actor},
    program::ReceiveInput,
};
use vesting_core::{Internal, Schedule, ScheduleSpec, Shape};

const PROGRAM: AccountId = AccountId::new([1; 32]);
const SCHEDULE: AccountId = AccountId::new([2; 32]);
const CREATOR: AccountId = AccountId::new([3; 32]);
const SOURCE: AccountId = AccountId::new([4; 32]);
const BENEFICIARY: AccountId = AccountId::new([5; 32]);

fn input(
    from: AccountId,
    is_authorized: bool,
    pre_state: Vec<u8>,
    message: &Internal,
) -> ReceiveInput {
    ReceiveInput {
        receiver: Actor::new(SCHEDULE, PROGRAM),
        from: Some(Actor::new(CREATOR, from)),
        is_authorized,
        pre_state: pre_state.try_into().expect("a small state"),
        message: borsh::to_vec(message).unwrap(),
    }
}

fn init(is_authorized: bool, pre_state: Vec<u8>) {
    let message = Internal::Init {
        creator: CREATOR,
        batch: 0,
        source: SOURCE,
        spec: ScheduleSpec {
            beneficiary: BENEFICIARY,
            total: 10,
            shape: Shape::Linear { start: 0, end: 10 },
            cancelable: true,
            transferable: false,
            cancel_authority: None,
            milestone_authority: None,
        },
    };
    let _ = vesting_program::handle_message(&input(PROGRAM, is_authorized, pre_state, &message));
}

#[test]
fn a_fresh_authorised_schedule_is_written() {
    init(true, vec![]);
}

#[test]
#[should_panic(expected = "E02")]
fn an_unauthorised_first_message_is_refused() {
    init(false, vec![]);
}

#[test]
#[should_panic(expected = "E02")]
fn an_existing_schedule_is_never_rewritten() {
    init(true, vec![1]);
}

/// Converts a schedule, as its creator, through the actor of `program`.
fn convert(program: AccountId) {
    let schedule = Schedule {
        creator: CREATOR,
        batch: 0,
        beneficiary: BENEFICIARY,
        source: SOURCE,
        total: 10,
        claimed: 0,
        shape: Shape::Linear { start: 0, end: 10 },
        cancelable: true,
        transferable: false,
        cancel_authority: CREATOR,
        milestone_authority: CREATOR,
        cancelled_at: None,
    };
    let state = borsh::to_vec(&schedule).unwrap();
    let message = Internal::MakeNonCancelable;
    let _ = vesting_program::handle_message(&input(program, false, state, &message));
}

#[test]
fn a_schedule_accepts_its_own_program() {
    convert(PROGRAM);
}

#[test]
#[should_panic(expected = "E02")]
fn a_schedule_refuses_another_program_acting_for_the_same_account() {
    convert(AccountId::new([9; 32]));
}
