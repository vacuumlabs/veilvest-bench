//! Cycles charged per transaction, and the largest batch within LEZ's 10M gas limit.
//! `RISC0_DEV_MODE=1 cargo test -q -p vesting_tests --test gas -- --ignored --nocapture --test-threads 1`

use fee_core::market::MAX_GAS_EXEC;
use lee::PublicTransaction;
use lee_core::account::AccountId;
use vesting_core::{
    seeds::{escrow_id, schedule_id},
    Message, ScheduleSpec, Shape,
};
use vesting_tests::*;

fn row(name: &str, chain: &Chain, tx: &PublicTransaction) {
    println!(
        "{name:<44} {:>10} cycles {:>8} bytes",
        chain.cycles(tx),
        borsh::to_vec(tx).expect("encodes").len()
    );
}

fn create_tx(chain: &Chain, people: &People, specs: &[ScheduleSpec]) -> PublicTransaction {
    let (declared, message) = chain.create_parts(people, 0, specs);
    chain.transaction(
        vesting(id(&people.creator)),
        &declared,
        &[&people.creator, &people.source],
        &message,
    )
}

fn batch(n: usize, shape: &Shape) -> Vec<ScheduleSpec> {
    (0..n)
        .map(|i| {
            let mut beneficiary = [7u8; 32];
            beneficiary[..8].copy_from_slice(&(i as u64).to_le_bytes());
            spec(AccountId::new(beneficiary), 100, shape.clone())
        })
        .collect()
}

fn shapes() -> [(&'static str, Shape); 4] {
    [
        ("linear", linear()),
        (
            "cliff + linear",
            Shape::CliffLinear {
                start: START,
                cliff: START + 250_000,
                end: END,
            },
        ),
        (
            "milestones, 4 tranches",
            Shape::Milestones {
                tranches: vec![25; 4],
                signalled: 0,
            },
        ),
        (
            "milestones, 20 tranches",
            Shape::Milestones {
                tranches: vec![5; 20],
                signalled: 0,
            },
        ),
    ]
}

#[test]
#[ignore = "benchmark"]
fn per_operation() {
    let people = people();
    let mut chain = Chain::funded(&people, 1_000_000);
    let (creator, source, bob) = (id(&people.creator), id(&people.source), id(&people.bob));
    let definition = id(&people.definition);
    println!("\nlimit: {MAX_GAS_EXEC}\n");
    for (name, shape) in shapes() {
        row(
            &format!("create, 1 schedule, {name}"),
            &chain,
            &create_tx(&chain, &people, &batch(1, &shape)),
        );
    }
    let escrow = escrow_id(VESTING, creator, 0);
    let at = |i| schedule_id(VESTING, creator, 0, i);
    let mut movable = spec(bob, 1_000, linear());
    movable.transferable = true;
    chain
        .create(
            &people,
            0,
            &[
                movable,
                spec(bob, 1_000, shapes()[1].1.clone()),
                spec(
                    bob,
                    100,
                    Shape::Milestones {
                        tranches: vec![25; 4],
                        signalled: 0,
                    },
                ),
            ],
        )
        .expect("created");
    chain.now = START + 500_000;
    let claim = |i: u32, to, open| Message::Claim {
        schedule: at(i),
        amount: 100,
        as_of: START + 500_000,
        to,
        open_holding: open,
    };
    let mut declared = act_declared(bob, at(0), &[escrow, bob]);
    declared.push(token(definition));
    let tx = chain.transaction(
        vesting(bob),
        &declared,
        &[&people.bob],
        &claim(0, bob, Some(definition)),
    );
    row("claim to public, opening the holding", &chain, &tx);
    chain.open(&people.bob, definition);
    for (i, name) in [(0, "linear"), (1, "cliff + linear")] {
        let tx = chain.transaction(
            vesting(bob),
            &act_declared(bob, at(i), &[escrow, bob]),
            &[&people.bob],
            &claim(i, bob, None),
        );
        row(&format!("claim to public, {name}"), &chain, &tx);
    }
    let as_creator = |message: Message, i: u32, tokens: &[AccountId]| {
        chain.transaction(
            vesting(creator),
            &act_declared(creator, at(i), tokens),
            &[&people.creator],
            &message,
        )
    };
    row(
        "cancel, nothing to refund",
        &chain,
        &as_creator(
            Message::Cancel {
                schedule: at(0),
                until: END,
            },
            0,
            &[escrow, source],
        ),
    );
    row(
        "cancel with refund",
        &chain,
        &as_creator(
            Message::Cancel {
                schedule: at(0),
                until: START + 600_000,
            },
            0,
            &[escrow, source],
        ),
    );
    row(
        "signal milestone",
        &chain,
        &as_creator(
            Message::SignalMilestone {
                schedule: at(2),
                index: 1,
            },
            2,
            &[],
        ),
    );
    row(
        "make non-cancelable",
        &chain,
        &as_creator(Message::MakeNonCancelable { schedule: at(0) }, 0, &[]),
    );
    let tx = chain.transaction(
        vesting(bob),
        &act_declared(bob, at(0), &[]),
        &[&people.bob],
        &Message::TransferBeneficiary {
            schedule: at(0),
            to: source,
        },
    );
    row("transfer beneficiary", &chain, &tx);
}

#[test]
#[ignore = "benchmark"]
fn batch_maximum() {
    let people = people();
    let chain = Chain::funded(&people, 1_000_000);
    let cases = shapes()
        .into_iter()
        .map(|(name, shape)| (name, shape, false))
        .chain([("linear, both roles named", linear(), true)]);
    for (name, shape, roles) in cases {
        let batch = |n| {
            let mut specs = batch(n, &shape);
            if roles {
                for spec in &mut specs {
                    spec.cancel_authority = Some(AccountId::new([8; 32]));
                    spec.milestone_authority = Some(AccountId::new([9; 32]));
                }
            }
            specs
        };
        println!("\n{name}");
        for n in [1, 8, 16, 32, 64, 128] {
            let tx = create_tx(&chain, &people, &batch(n));
            println!(
                "{n:>5} {:>10} cycles {:>8} bytes",
                chain.cycles(&tx),
                borsh::to_vec(&tx).expect("encodes").len()
            );
        }
        let fits = |n| chain.fits(&create_tx(&chain, &people, &batch(n)), MAX_GAS_EXEC);
        let (mut low, mut high) = (1, 2_048);
        while low + 1 < high {
            let mid = (low + high) / 2;
            if fits(mid) {
                low = mid;
            } else {
                high = mid;
            }
        }
        let tx = create_tx(&chain, &people, &batch(low));
        println!(
            "max {low} ({} cycles, {} bytes); {high} does not fit",
            chain.cycles(&tx),
            borsh::to_vec(&tx).expect("encodes").len()
        );
        // Applied, not only within the budget.
        let mut applied = chain.clone();
        applied
            .state
            .transition_from_public_transaction(&tx, 1, START)
            .expect("the largest batch applies");
    }
}
