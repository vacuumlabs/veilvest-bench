//! Private claims through LEZ's privacy circuit and settlement. With `RISC0_DEV_MODE=1` the
//! circuit executes but its receipt is not a real proof; unset, the same tests prove for real.

use std::collections::BTreeSet;

use lee::{
    error::LeeError,
    privacy_preserving_transaction::{
        circuit::{execute_and_prove, ProgramCatalog, Simulation},
        Message as PrivateMessage, WitnessSet,
    },
    program::Program,
    test_utils::{init_witness, no_seal, update_witness, TestPrivateKeys},
    PrivacyPreservingTransaction, PrivateKey, ValidatedStateDiff,
};
use lee_core::{
    account::{Account, AccountId, Actor},
    execution_state::{PublicExecutionContext, TransactionEntry},
    program::TransactionEvent,
    AuthorizationSecretKey, Commitment, EncryptionScheme, ProvingInput, RegularKey, RootCall,
    SenderPresentation, SharedSecretKey, WitnessKind,
};
use token_core::TokenState;
use vesting_core::{
    error, event,
    seeds::{escrow_id, schedule_id},
    Message, TOKEN_PROGRAM,
};
use vesting_tests::*;

struct Wallet {
    keys: TestPrivateKeys,
    account: Option<Account>,
}

impl Wallet {
    fn new(byte: u8) -> Self {
        Self {
            keys: TestPrivateKeys {
                ask: AuthorizationSecretKey([byte; 32]),
                d: [byte; 32],
                z: [byte; 32],
            },
            account: None,
        }
    }

    fn id(&self) -> AccountId {
        self.keys.account_id()
    }

    fn balance(&self) -> u128 {
        let state = self.account.as_ref().expect("the account exists");
        match TokenState::decode(state.data.actor_state(TOKEN_PROGRAM)) {
            Some(TokenState::Holding(holding)) => holding.balance,
            other => panic!("a private holding, got {other:?}"),
        }
    }

    /// Finds this wallet's new state in a transaction's notes.
    fn receive(&mut self, tx: &PrivacyPreservingTransaction) {
        let execution = &tx.message().execution;
        let commitments = execution.commitments();
        self.account = Some(
            execution
                .private_actions
                .iter()
                .find_map(|action| {
                    let secret = SharedSecretKey::decapsulate(
                        &action.encrypted_post_state.epk,
                        &self.keys.d,
                        &self.keys.z,
                    )?;
                    let (_, state) = EncryptionScheme::decrypt(
                        &action.encrypted_post_state.ciphertext,
                        &secret,
                        &action.nullifier,
                    )?;
                    commitments
                        .contains(&Commitment::new(&self.id(), &state))
                        .then_some(state)
                })
                .expect("the wallet decrypts its note"),
        );
    }
}

struct Claim<'a> {
    who: &'a PrivateKey,
    wallet: &'a Wallet,
    factor: [u8; 32],
    schedule: AccountId,
    escrow: AccountId,
    definition: AccountId,
    amount: u128,
    /// Creates the private holding in the same transaction.
    open: bool,
    promote: bool,
}

fn programs() -> ProgramCatalog {
    [
        (VESTING, vesting_methods::VESTING_ELF),
        (TOKEN_PROGRAM, token_methods::TOKEN_ELF),
    ]
    .into_iter()
    .map(|(id, elf)| (id, Program::new(elf.to_vec().into()).expect("loads")))
    .collect()
}

/// The public actors as the chain holds them now, for the prover to run against.
fn simulation(chain: &Chain, public: &[Actor]) -> Simulation {
    Simulation {
        public_actor_states: public
            .iter()
            .map(|actor| {
                let account = chain.state.get_account_by_id(actor.account_id);
                let state = account.data.actor_state(actor.program_account_id);
                (*actor, state.clone())
            })
            .collect(),
        admitted_accounts: None,
    }
}

/// Builds and proves a private claim against `chain` as it is now.
fn prove(chain: &Chain, claim: &Claim<'_>) -> Result<PrivacyPreservingTransaction, LeeError> {
    let alias = claim.wallet.id().blinded(&claim.factor);
    let mut witness = match &claim.wallet.account {
        None => init_witness(&claim.wallet.keys),
        Some(state) => update_witness(
            &claim.wallet.keys,
            state.clone(),
            chain
                .state
                .get_proof_for_commitment(&Commitment::new(&claim.wallet.id(), state))
                .expect("the current note is committed"),
        ),
    };
    if !claim.open {
        // Receiving needs no authorization by the private account.
        witness.kind = WitnessKind::Regular(RegularKey::Nullifying(claim.wallet.keys.nsk()));
    }
    witness.address_openings = BTreeSet::from([claim.factor]);

    let beneficiary = id(claim.who);
    let mut public = act_declared(beneficiary, claim.schedule, &[claim.escrow]);
    if claim.open {
        public.push(token(claim.definition));
    }
    let message = Message::Claim {
        schedule: claim.schedule,
        amount: claim.amount,
        as_of: chain.now,
        to: alias,
        open_holding: claim.open.then_some(claim.definition),
    };
    let factor = claim.factor;
    let (output, proof) = execute_and_prove(
        ProvingInput {
            root: TransactionEntry::Sync(RootCall {
                to: vesting(beneficiary),
                message: borsh::to_vec(&message).expect("encodes"),
            }),
            claims: Vec::new(),
            fee: None,
            public_context: PublicExecutionContext::new(public.clone(), [beneficiary]),
            private_witnesses: vec![witness],
            dummy_inputs: Vec::new(),
            ciphertext_padding: None,
            recoveries: Vec::new(),
            private_promotions: BTreeSet::new(),
        },
        &simulation(chain, &public),
        &programs(),
        |_| SenderPresentation::Blinded(factor),
        |_, body| claim.promote && body.to.account_id == alias,
        no_seal,
    )?;
    let nonces = [(
        beneficiary,
        chain.state.get_account_by_id(beneficiary).nonce,
    )]
    .into();
    let message = PrivateMessage::from_circuit_output(nonces, output);
    let witnesses = WitnessSet::for_message(&message, proof, &[claim.who]);
    Ok(PrivacyPreservingTransaction::new(message, witnesses))
}

fn settle(
    chain: &mut Chain,
    tx: &PrivacyPreservingTransaction,
) -> Result<Vec<TransactionEvent>, LeeError> {
    let diff = ValidatedStateDiff::from_privacy_preserving_transaction(
        tx,
        &chain.state,
        chain.block,
        chain.now,
    )?;
    chain.state.apply_state_diff(diff)
}

struct Setup {
    chain: Chain,
    people: People,
    escrow: AccountId,
    bobs: AccountId,
    carols: AccountId,
}

/// Bob and carol each have 1,000 vesting linearly from one escrow; half has vested.
fn setup() -> Setup {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    let mut bobs = spec(id(&people.bob), 1_000, linear());
    bobs.transferable = true;
    chain
        .create(
            &people,
            0,
            &[bobs, spec(id(&people.carol), 1_000, linear())],
        )
        .expect("created");
    chain.now = START + 500_000;
    let creator = id(&people.creator);
    Setup {
        chain,
        escrow: escrow_id(VESTING, creator, 0),
        bobs: schedule_id(VESTING, creator, 0, 0),
        carols: schedule_id(VESTING, creator, 0, 1),
        people,
    }
}

impl Setup {
    fn claim<'a>(
        &'a self,
        who: &'a PrivateKey,
        wallet: &'a Wallet,
        factor: u8,
        schedule: AccountId,
        amount: u128,
        open: bool,
    ) -> Claim<'a> {
        Claim {
            who,
            wallet,
            factor: [factor; 32],
            schedule,
            escrow: self.escrow,
            definition: id(&self.people.definition),
            amount,
            open,
            promote: true,
        }
    }

    fn bob_claims<'a>(
        &'a self,
        wallet: &'a Wallet,
        factor: u8,
        amount: u128,
        open: bool,
    ) -> Claim<'a> {
        self.claim(&self.people.bob, wallet, factor, self.bobs, amount, open)
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// One transaction: a fresh private account, its holding created and credited, and nothing public
/// names it.
#[test]
fn the_first_claim_opens_and_credits_a_private_holding_in_one_transaction() {
    let mut s = setup();
    let mut wallet = Wallet::new(50);
    let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 300, true)).expect("proved");
    let events = settle(&mut s.chain, &tx).expect("settled");
    wallet.receive(&tx);
    assert_eq!(wallet.balance(), 300);
    assert_eq!(s.chain.balance(s.escrow), 1_700);
    assert_eq!(s.chain.schedule(s.bobs).unwrap().claimed, 300);
    assert_eq!(events_of::<event::Claimed>(&events)[0].amount, 300);

    let mut public = borsh::to_vec(&tx).expect("encodes");
    for record in &events {
        public.extend_from_slice(&record.event.data);
    }
    assert!(!contains(&public, wallet.id().as_ref()), "the account id");
    assert!(!contains(&public, &[60; 32]), "the blinding factor");
    assert!(!contains(&public, &wallet.keys.npk().0), "the key");
    let alias = wallet.id().blinded(&[60; 32]);
    assert!(contains(&public, alias.as_ref()), "only the alias");
}

#[test]
fn later_claims_reach_the_same_account_at_unrelated_addresses() {
    let mut s = setup();
    let mut wallet = Wallet::new(50);
    let first = prove(&s.chain, &s.bob_claims(&wallet, 60, 300, true)).expect("proved");
    settle(&mut s.chain, &first).expect("settled");
    wallet.receive(&first);
    let second = prove(&s.chain, &s.bob_claims(&wallet, 61, 200, false)).expect("proved");
    settle(&mut s.chain, &second).expect("settled");
    wallet.receive(&second);
    assert_eq!(wallet.balance(), 500);
    let bytes = borsh::to_vec(&second).expect("encodes");
    assert!(!contains(&bytes, wallet.id().blinded(&[60; 32]).as_ref()));
    // A later claim does not name the token definition either.
    assert!(!tx_declares(&second, token(id(&s.people.definition))));
}

fn tx_declares(tx: &PrivacyPreservingTransaction, actor: Actor) -> bool {
    tx.message().public_context.actors.contains(&actor)
}

/// Proofs made against one escrow settle in any order, with other claims in between.
#[test]
fn proofs_on_one_escrow_settle_in_any_order() {
    let mut s = setup();
    let (mut bobs, mut carols) = (Wallet::new(50), Wallet::new(51));
    let bob = prove(&s.chain, &s.bob_claims(&bobs, 60, 500, true)).expect("proved");
    let carol = prove(
        &s.chain,
        &s.claim(&s.people.carol, &carols, 61, s.carols, 500, true),
    )
    .expect("proved");
    // A new batch and a few blocks pass meanwhile.
    s.chain
        .create(&s.people, 1, &[spec(id(&s.people.other), 5, linear())])
        .expect("created");
    s.chain.now += 60_000;
    s.chain.block += 5;
    settle(&mut s.chain, &carol).expect("carol's settles first");
    settle(&mut s.chain, &bob).expect("bob's, unchanged");
    bobs.receive(&bob);
    carols.receive(&carol);
    assert_eq!((bobs.balance(), carols.balance()), (500, 500));
    assert_eq!(s.chain.balance(s.escrow), 1_000);
}

#[test]
fn a_cancel_while_proving_does_not_stop_the_claim() {
    let mut s = setup();
    let mut wallet = Wallet::new(50);
    let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 500, true)).expect("proved");
    s.chain.now += 60_000;
    let (until, source, escrow, bobs) = (s.chain.now + 1, id(&s.people.source), s.escrow, s.bobs);
    s.chain
        .cancel(&s.people.creator, bobs, escrow, source, until)
        .expect("cancelled");
    settle(&mut s.chain, &tx).expect("the claim still settles");
    wallet.receive(&tx);
    assert_eq!(wallet.balance(), 500);
    // 1,000 - 560 vested went back; 60 of bob's and carol's 1,000 remain.
    assert_eq!(s.chain.balance(s.escrow), 1_060);
}

/// The same proof, signed again at the signer's current nonce.
fn sign_again(
    chain: &Chain,
    tx: &PrivacyPreservingTransaction,
    who: &PrivateKey,
) -> PrivacyPreservingTransaction {
    let mut message = tx.message().clone();
    message.nonces = [(id(who), chain.state.get_account_by_id(id(who)).nonce)].into();
    let witnesses = WitnessSet::for_message(&message, tx.witness_set().proof().clone(), &[who]);
    PrivacyPreservingTransaction::new(message, witnesses)
}

/// The beneficiary's own transactions move the nonce. That costs a signature, not a proof: the
/// same proof settles as long as both amounts fit.
#[test]
fn a_public_claim_while_proving_needs_a_new_signature_not_a_new_proof() {
    for (public, fits) in [(200, true), (201, false)] {
        let mut s = setup();
        let mut wallet = Wallet::new(50);
        let bob = id(&s.people.bob);
        s.chain.open(&s.people.bob, id(&s.people.definition));
        let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 300, true)).expect("proved");
        let (now, bobs, escrow) = (s.chain.now, s.bobs, s.escrow);
        s.chain
            .claim(&s.people.bob, bobs, escrow, public, now, bob)
            .expect("the public claim");
        let stale = settle(&mut s.chain, &tx);
        assert_refused(&stale, "Nonce mismatch");
        let again = sign_again(&s.chain, &tx, &s.people.bob);
        let result = settle(&mut s.chain, &again);
        if fits {
            result.expect("the same proof settles");
            wallet.receive(&again);
            assert_eq!(wallet.balance(), 300);
        } else {
            assert_refused(&result, error::EXCEEDS_VESTED);
            assert_eq!(s.chain.schedule(bobs).unwrap().claimed, public);
        }
    }
}

#[test]
fn a_refused_private_claim_consumes_nothing() {
    let mut s = setup();
    let wallet = Wallet::new(50);
    let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 500, true)).expect("proved");
    // Bob gives the position away before it settles.
    let (bobs, to) = (s.bobs, id(&s.people.other));
    s.chain
        .act(
            &s.people.bob,
            bobs,
            &[],
            &Message::TransferBeneficiary { schedule: bobs, to },
        )
        .expect("transferred");
    let before = s.chain.state.clone();
    let again = sign_again(&s.chain, &tx, &s.people.bob);
    assert_refused(&settle(&mut s.chain, &again), error::NOT_BENEFICIARY);
    assert!(s.chain.state == before, "nothing changed");
}

#[test]
fn a_claim_for_the_future_does_not_settle_before_its_time() {
    let mut s = setup();
    let mut wallet = Wallet::new(50);
    let now = s.chain.now;
    s.chain.now += 100_000;
    let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 600, true)).expect("proved for later");
    s.chain.now = now;
    assert_refused(&settle(&mut s.chain, &tx), "OutOfValidityWindow");
    s.chain.now += 100_000;
    settle(&mut s.chain, &tx).expect("settles at its time");
    wallet.receive(&tx);
    assert_eq!(wallet.balance(), 600);
}

#[test]
fn a_proof_is_bound_to_its_amount_and_destination() {
    let s = setup();
    let wallet = Wallet::new(50);
    let tx = prove(&s.chain, &s.bob_claims(&wallet, 60, 300, true)).expect("proved");
    let thief = Wallet::new(52);
    for message in [
        Message::Claim {
            schedule: s.bobs,
            amount: 301,
            as_of: s.chain.now,
            to: wallet.id().blinded(&[60; 32]),
            open_holding: Some(id(&s.people.definition)),
        },
        Message::Claim {
            schedule: s.bobs,
            amount: 300,
            as_of: s.chain.now,
            to: thief.id().blinded(&[60; 32]),
            open_holding: Some(id(&s.people.definition)),
        },
    ] {
        let mut altered = tx.message().clone();
        altered.execution.public_root = Some(RootCall {
            to: vesting(id(&s.people.bob)),
            message: borsh::to_vec(&message).expect("encodes"),
        });
        let witnesses =
            WitnessSet::for_message(&altered, tx.witness_set().proof().clone(), &[&s.people.bob]);
        let mut chain = s.chain.clone();
        let altered = PrivacyPreservingTransaction::new(altered, witnesses);
        let result = settle(&mut chain, &altered);
        assert_refused(&result, "InvalidPrivacyPreservingProof");
        assert_eq!(chain.schedule(s.bobs).unwrap().claimed, 0);
    }
}

/// A destination whose owner does not open it in the proof cannot be credited in place.
#[test]
fn a_private_destination_must_be_opened_in_the_proof() {
    let s = setup();
    let wallet = Wallet::new(50);
    let mut claim = s.bob_claims(&wallet, 60, 300, true);
    claim.promote = false;
    let tx = prove(&s.chain, &claim).expect("the prover builds it");
    let mut chain = s.chain.clone();
    assert_refused(&settle(&mut chain, &tx), "UnboundAsyncDestination");
    assert_eq!(chain.schedule(s.bobs).unwrap().claimed, 0);
}

/// A private account reaches other actors under an address the prover chooses. Were a creator
/// stored under such an address, its schedules would point at an escrow that does not exist.
#[test]
fn a_private_creator_under_a_one_time_address_is_refused() {
    let people = people();
    let mut chain = Chain::funded(&people, 10_000);
    chain.open(&people.bob, id(&people.definition));
    let wallet = Wallet::new(70);
    let (creator, source) = (wallet.id(), id(&people.source));
    let definition = id(&people.definition);
    let escrow = escrow_id(VESTING, creator, 0);
    let schedule = schedule_id(VESTING, creator, 0, 0);
    let public = vec![
        token(source),
        token(definition),
        token(escrow),
        vesting(schedule),
    ];
    let message = Message::CreateBatch {
        token: definition,
        source,
        schedules: vec![spec(id(&people.bob), 1_000, linear())],
    };
    let mut create = |canonical: bool| {
        let mut factor = 0u8;
        let mut witness = init_witness(&wallet.keys);
        witness.kind = WitnessKind::Regular(RegularKey::Authorized(wallet.keys.ask));
        let (output, proof) = execute_and_prove(
            ProvingInput {
                root: TransactionEntry::Sync(RootCall {
                    to: vesting(creator),
                    message: borsh::to_vec(&message).expect("encodes"),
                }),
                claims: Vec::new(),
                fee: None,
                public_context: PublicExecutionContext::new(public.clone(), [source]),
                private_witnesses: vec![witness],
                dummy_inputs: Vec::new(),
                ciphertext_padding: None,
                recoveries: Vec::new(),
                private_promotions: BTreeSet::new(),
            },
            &simulation(&chain, &public),
            &programs(),
            |_| {
                if canonical {
                    return SenderPresentation::Canonical;
                }
                factor += 1;
                SenderPresentation::Blinded([factor; 32])
            },
            |_, _| false,
            no_seal,
        )?;
        let nonces = [(source, chain.state.get_account_by_id(source).nonce)].into();
        let message = PrivateMessage::from_circuit_output(nonces, output);
        let witnesses = WitnessSet::for_message(&message, proof, &[&people.source]);
        settle(
            &mut chain,
            &PrivacyPreservingTransaction::new(message, witnesses),
        )
    };
    let result = create(false);
    assert_refused(&result, error::NOT_CREATOR);
    create(true).expect("a creator shown under its own address is accepted");
    assert_eq!(chain.schedule(schedule).unwrap().creator, creator);
    chain.now = END;
    chain
        .claim(&people.bob, schedule, escrow, 1_000, END, id(&people.bob))
        .expect("and its schedule pays");
}

/// Real proofs of a first and a later claim; prints the time of each.
/// `cargo test -p vesting_tests --test private real_proofs -- --ignored --nocapture`
#[test]
#[ignore = "minutes of proving"]
fn real_proofs() {
    assert!(
        std::env::var("RISC0_DEV_MODE").is_err(),
        "unset RISC0_DEV_MODE for real proofs"
    );
    let mut s = setup();
    let mut wallet = Wallet::new(50);
    for (name, factor, open) in [
        ("first claim, opening the holding", 60, true),
        ("later claim", 61, false),
    ] {
        let started = std::time::Instant::now();
        let tx = prove(&s.chain, &s.bob_claims(&wallet, factor, 200, open)).expect("proved");
        println!(
            "{name}: {:.0} s, {} bytes",
            started.elapsed().as_secs_f64(),
            borsh::to_vec(&tx).expect("encodes").len()
        );
        settle(&mut s.chain, &tx).expect("a real proof settles");
        wallet.receive(&tx);
    }
    assert_eq!(wallet.balance(), 400);
}
