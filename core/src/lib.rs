//! Messages, state, accrual and addresses of the vesting program. No I/O.

pub mod accrual;
pub mod event;
pub mod seeds;

use borsh::{BorshDeserialize, BorshSerialize};
use lee_core::account::AccountId;

pub use accrual::Shape;

/// The token program every escrow lives in. Fixed here, so a schedule cannot name a look-alike.
/// In this bench it is the address the test chain deploys the 0.4 token port at.
pub const TOKEN_PROGRAM: AccountId = AccountId::new([0xa2; 32]);

/// Unix milliseconds, as LEZ block timestamps. Defined here so that the IDL generator resolves it.
pub type Timestamp = u64;

/// How far ahead of the block a cancel may place its split: one hour, in milliseconds.
pub const MAX_CANCEL_LEAD: Timestamp = 3_600_000;

/// Most tranches of a milestone schedule: one bit each in `signalled`.
pub const MAX_TRANCHES: usize = 64;

/// Refusals, as the panic message of the handler. Clients map the prefix to a typed error.
pub mod error {
    pub const NOT_AUTHORIZED: &str = "E01 NotAuthorized";
    pub const UNEXPECTED: &str = "E02 UnexpectedMessage";
    pub const BAD_SCHEDULE: &str = "E03 InvalidSchedule";
    pub const NOT_BENEFICIARY: &str = "E04 NotBeneficiary";
    pub const ZERO_AMOUNT: &str = "E05 ZeroAmount";
    pub const EXCEEDS_VESTED: &str = "E06 AmountExceedsVested";
    pub const NOT_CANCELABLE: &str = "E07 NotCancelable";
    pub const ALREADY_CANCELLED: &str = "E08 AlreadyCancelled";
    pub const NOT_CANCEL_AUTHORITY: &str = "E09 NotCancelAuthority";
    pub const NOT_MILESTONE_AUTHORITY: &str = "E10 NotMilestoneAuthority";
    pub const UNKNOWN_MILESTONE: &str = "E11 UnknownMilestone";
    pub const ALREADY_SIGNALLED: &str = "E12 MilestoneAlreadySignalled";
    pub const NOT_TRANSFERABLE: &str = "E13 NotTransferable";
    pub const NOT_CREATOR: &str = "E14 NotCreator";
}

/// One schedule of a batch, as the creator specifies it.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ScheduleSpec {
    pub beneficiary: AccountId,
    pub total: u128,
    pub shape: Shape,
    pub cancelable: bool,
    pub transferable: bool,
    /// `None` is the creator.
    pub cancel_authority: Option<AccountId>,
    pub milestone_authority: Option<AccountId>,
}

#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Schedule {
    pub creator: AccountId,
    pub batch: u64,
    pub beneficiary: AccountId,
    /// The holding that funded the batch; refunds go back to it.
    pub source: AccountId,
    pub total: u128,
    pub claimed: u128,
    pub shape: Shape,
    pub cancelable: bool,
    pub transferable: bool,
    pub cancel_authority: AccountId,
    pub milestone_authority: AccountId,
    /// The time a cancel split at.
    pub cancelled_at: Option<Timestamp>,
}

impl Schedule {
    /// What has vested at `at`, never counting time after a cancel's split.
    #[must_use]
    pub fn vested(&self, at: Timestamp) -> u128 {
        let at = self.cancelled_at.map_or(at, |split| at.min(split));
        self.shape.vested(self.total, at)
    }

    #[must_use]
    pub fn claimable(&self, at: Timestamp) -> u128 {
        self.vested(at).saturating_sub(self.claimed)
    }
}

/// The creator-side state of a person's own vesting actor.
#[derive(Debug, Clone, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CreatorState {
    pub next_batch: u64,
}

/// To a person's own vesting actor, from the transaction or from a program that holds a role.
/// The actor's account must authorise it.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Message {
    CreateBatch {
        token: AccountId,
        /// The holding that funds the batch; it must authorise too.
        source: AccountId,
        schedules: Vec<ScheduleSpec>,
    },
    Claim {
        schedule: AccountId,
        amount: u128,
        as_of: Timestamp,
        to: AccountId,
        /// `Some(token)` creates the destination holding first.
        open_holding: Option<AccountId>,
    },
    Cancel {
        schedule: AccountId,
        until: Timestamp,
    },
    MakeNonCancelable {
        schedule: AccountId,
    },
    SignalMilestone {
        schedule: AccountId,
        index: u8,
    },
    TransferBeneficiary {
        schedule: AccountId,
        to: AccountId,
    },
}

/// Between this program's own actors. Only its code can produce these.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Internal {
    Init {
        /// The account the schedule's address is derived from; the sender must be shown as it.
        creator: AccountId,
        batch: u64,
        source: AccountId,
        spec: ScheduleSpec,
    },
    Claim {
        amount: u128,
        as_of: Timestamp,
        to: AccountId,
    },
    Cancel {
        until: Timestamp,
    },
    MakeNonCancelable,
    SignalMilestone {
        index: u8,
    },
    TransferBeneficiary {
        to: AccountId,
    },
}
