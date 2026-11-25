//! A chain state with the real vesting and token guests, run through LEZ's own execution.

use std::borrow::Cow;

use borsh::BorshSerialize;
use lee::{
    error::LeeError,
    program::Program,
    public_transaction::{Message as TxMessage, WitnessSet},
    PrivateKey, PublicKey, PublicTransaction, V03State, ValidatedStateDiff,
};
use lee_core::{
    account::{AccountId, Actor},
    program::TransactionEvent,
    Timestamp,
};
use token_core::{TokenHolding, TokenState};
use vesting_core::{
    event::{selector, VestingEvent},
    seeds, Message, Schedule, ScheduleSpec, Shape, TOKEN_PROGRAM,
};

pub const VESTING: AccountId = AccountId::new([0xa1; 32]);
pub const START: Timestamp = 1_000_000;
pub const END: Timestamp = 2_000_000;

pub fn key(byte: u8) -> PrivateKey {
    PrivateKey::try_new([byte; 32]).expect("a valid test key")
}

pub fn id(key: &PrivateKey) -> AccountId {
    AccountId::from(&PublicKey::new_from_private_key(key))
}

pub fn vesting(account: AccountId) -> Actor {
    Actor::new(account, VESTING)
}

pub fn token(account: AccountId) -> Actor {
    Actor::new(account, TOKEN_PROGRAM)
}

pub fn linear() -> Shape {
    Shape::Linear {
        start: START,
        end: END,
    }
}

pub fn spec(beneficiary: AccountId, total: u128, shape: Shape) -> ScheduleSpec {
    ScheduleSpec {
        beneficiary,
        total,
        shape,
        cancelable: true,
        transferable: false,
        cancel_authority: None,
        milestone_authority: None,
    }
}

#[derive(Clone)]
pub struct Chain {
    pub state: V03State,
    pub now: Timestamp,
    pub block: u64,
}

/// The people of the tests. The creator funds from a separate holding account.
pub struct People {
    pub definition: PrivateKey,
    pub creator: PrivateKey,
    pub source: PrivateKey,
    pub bob: PrivateKey,
    pub carol: PrivateKey,
    pub other: PrivateKey,
}

pub fn people() -> People {
    People {
        definition: key(10),
        creator: key(11),
        source: key(12),
        bob: key(13),
        carol: key(14),
        other: key(15),
    }
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

impl Chain {
    pub fn new() -> Self {
        let state = V03State::new().with_named_programs([
            (
                VESTING,
                Program::new(Cow::Borrowed(vesting_methods::VESTING_ELF)).expect("vesting loads"),
            ),
            (
                TOKEN_PROGRAM,
                Program::new(Cow::Borrowed(token_methods::TOKEN_ELF)).expect("token loads"),
            ),
        ]);
        Self {
            state,
            now: START,
            block: 1,
        }
    }

    /// A chain where `people.source` holds `supply` of the token `people.definition`.
    pub fn funded(people: &People, supply: u128) -> Self {
        let mut chain = Self::new();
        chain.define(&people.definition, &people.source, supply);
        chain
    }

    pub fn define(&mut self, definition: &PrivateKey, holder: &PrivateKey, supply: u128) {
        self.submit(
            token(id(definition)),
            &[token(id(definition)), token(id(holder))],
            &[definition, holder],
            &token_core::Message::NewDefinition {
                name: "Test".into(),
                total_supply: supply,
                mint_authority: None,
                holding: id(holder),
            },
        )
        .expect("the token is created");
    }

    pub fn open(&mut self, holder: &PrivateKey, definition: AccountId) {
        self.submit(
            token(id(holder)),
            &[token(id(holder)), token(definition)],
            &[holder],
            &token_core::Message::InitializeHolding { definition },
        )
        .expect("the holding is initialized");
    }

    pub fn transaction(
        &self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
    ) -> PublicTransaction {
        let nonces = signers
            .iter()
            .map(|key| (id(key), self.state.get_account_by_id(id(key)).nonce))
            .collect();
        let tx = TxMessage::try_new(root, declared.iter().copied(), nonces, message)
            .expect("the message encodes");
        let witness = WitnessSet::for_message(&tx, signers);
        PublicTransaction::new(tx, witness)
    }

    pub fn submit(
        &mut self,
        root: Actor,
        declared: &[Actor],
        signers: &[&PrivateKey],
        message: &impl BorshSerialize,
    ) -> Result<Vec<TransactionEvent>, LeeError> {
        let tx = self.transaction(root, declared, signers, message);
        self.state
            .transition_from_public_transaction(&tx, self.block, self.now)
    }

    /// Cycles a transaction is charged, with the budget lifted.
    pub fn cycles(&self, tx: &PublicTransaction) -> u64 {
        ValidatedStateDiff::from_public_transaction_with_cycle_budget(
            tx,
            &self.state,
            self.block,
            self.now,
            1 << 36,
        )
        .map(|(_, charge)| charge.cycles)
        .expect("the transaction executes")
    }

    pub fn fits(&self, tx: &PublicTransaction, budget: u64) -> bool {
        ValidatedStateDiff::from_public_transaction_with_cycle_budget(
            tx,
            &self.state,
            self.block,
            self.now,
            budget,
        )
        .is_ok()
    }

    // ---- vesting ----

    pub fn create_parts(
        &self,
        people: &People,
        batch: u64,
        schedules: &[ScheduleSpec],
    ) -> (Vec<Actor>, Message) {
        let creator = id(&people.creator);
        let mut declared = vec![
            vesting(creator),
            token(id(&people.source)),
            token(id(&people.definition)),
            token(seeds::escrow_id(VESTING, creator, batch)),
        ];
        declared.extend(
            (0..schedules.len() as u32)
                .map(|index| vesting(seeds::schedule_id(VESTING, creator, batch, index))),
        );
        let message = Message::CreateBatch {
            token: id(&people.definition),
            source: id(&people.source),
            schedules: schedules.to_vec(),
        };
        (declared, message)
    }

    pub fn create(
        &mut self,
        people: &People,
        batch: u64,
        schedules: &[ScheduleSpec],
    ) -> Result<Vec<TransactionEvent>, LeeError> {
        let (declared, message) = self.create_parts(people, batch, schedules);
        self.submit(
            vesting(id(&people.creator)),
            &declared,
            &[&people.creator, &people.source],
            &message,
        )
    }

    pub fn act(
        &mut self,
        who: &PrivateKey,
        schedule: AccountId,
        tokens: &[AccountId],
        message: &Message,
    ) -> Result<Vec<TransactionEvent>, LeeError> {
        let declared = act_declared(id(who), schedule, tokens);
        self.submit(vesting(id(who)), &declared, &[who], message)
    }

    pub fn claim(
        &mut self,
        who: &PrivateKey,
        schedule: AccountId,
        escrow: AccountId,
        amount: u128,
        as_of: Timestamp,
        to: AccountId,
    ) -> Result<Vec<TransactionEvent>, LeeError> {
        self.act(
            who,
            schedule,
            &[escrow, to],
            &Message::Claim {
                schedule,
                amount,
                as_of,
                to,
                open_holding: None,
            },
        )
    }

    pub fn cancel(
        &mut self,
        who: &PrivateKey,
        schedule: AccountId,
        escrow: AccountId,
        refund_to: AccountId,
        until: Timestamp,
    ) -> Result<Vec<TransactionEvent>, LeeError> {
        self.act(
            who,
            schedule,
            &[escrow, refund_to],
            &Message::Cancel { schedule, until },
        )
    }

    pub fn schedule(&self, id: AccountId) -> Option<Schedule> {
        let account = self.state.get_account_by_id(id);
        let state = account.data.actor_state(VESTING);
        (!state.is_empty()).then(|| borsh::from_slice(state).expect("a schedule"))
    }

    pub fn holding(&self, id: AccountId) -> Option<TokenHolding> {
        match TokenState::decode(
            self.state
                .get_account_by_id(id)
                .data
                .actor_state(TOKEN_PROGRAM),
        ) {
            Some(TokenState::Holding(holding)) => Some(holding),
            Some(TokenState::Definition(_)) | None => None,
        }
    }

    pub fn balance(&self, id: AccountId) -> u128 {
        self.holding(id).map_or(0, |holding| holding.balance)
    }
}

pub fn act_declared(who: AccountId, schedule: AccountId, tokens: &[AccountId]) -> Vec<Actor> {
    let mut declared = vec![vesting(who), vesting(schedule)];
    declared.extend(tokens.iter().copied().map(token));
    declared
}

pub fn events_of<E: VestingEvent + borsh::BorshDeserialize>(events: &[TransactionEvent]) -> Vec<E> {
    events
        .iter()
        .filter(|record| record.event.selector == selector(E::NAME))
        .map(|record| borsh::from_slice(&record.event.data).expect("the event decodes"))
        .collect()
}

pub fn assert_refused<T: std::fmt::Debug>(result: &Result<T, LeeError>, reason: &str) {
    match result {
        Err(error) if format!("{error:?}").contains(reason) => {}
        other => panic!("expected a refusal with {reason:?}, got {other:?}"),
    }
}
