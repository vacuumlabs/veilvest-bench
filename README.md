# veilvest-bench

A minimal token vesting program for [LEZ](https://github.com/logos-blockchain/logos-execution-zone)
0.4, with the tests and measurements behind Vacuumlabs' Veilvest proposal for Logos RFP-017.

It is a design check and not the product. It has no SDK, CLI or app, no transfer of roles, one error
code for every malformed schedule, and no audit.

## What it shows

- **A claim to a private account is one transaction.** It credits a fresh one-time address of the
  account, and creates the account's holding the first time. Two claims with real proofs settled.
- **What others do while a claim is being proven does not invalidate it.** Tested for a cancel of
  the same schedule, other claims from the same escrow in any order, a new batch and the passing of
  time. When the beneficiary's own transaction moves the nonce, the same proof is signed again.
- **No clock is read.** A claim states the time it is computed for and a cancel the time it splits
  at. The transaction's validity window holds both to the block's time.
- **One escrow per batch.** Up to 434 linear schedules fit in one transaction of 10M gas.

## Results

| File | What it is |
|---|---|
| `results/gas.txt` | Cycles and bytes per operation, and the largest batch of each shape |
| `results/real-proofs.txt` | Two private claims, proven and settled on a desktop CPU: 264 s and 215 s |
| `results/faults.txt` | Single faults put into the program, and the tests that each one fails |
| `results/vesting-idl.json` | The IDL that SPEL's generator produces from `core` |

Cycles are what LEZ charges as execution gas for the whole transaction. No fee is declared, and a
fee declaration would add bytes. The guests are built locally, and the build embeds source paths, so
counts on another machine may differ slightly.

## Layout

| Path | What it is |
|---|---|
| `core` | Messages, state, accrual, events, addresses |
| `program` | The handlers, on LEZ's actor interface |
| `methods` | The RISC Zero guest |
| `tests` | The vesting and token guests on a LEZ `V03State` |
| `faults.py` | Puts each fault into a copy of the bench and runs the tests |

The tests use LEZ's own public execution, privacy circuit and settlement. They have no sequencer, no
network and no fees.

## Running it

Needs Rust 1.94 and RISC Zero 3.0.5 (`rzup install r0vm 3.0.5` and `rzup install rust`). That RISC
Zero version has been yanked from crates.io; the committed lockfiles still resolve it.

```
# every test; the privacy circuit runs, its receipt is not a real proof
RISC0_DEV_MODE=1 cargo test

# cycles and the largest batches, about 11 minutes
RISC0_DEV_MODE=1 cargo test -q -p vesting_tests --test gas -- --ignored --nocapture --test-threads 1

# two real proofs, about 8 minutes and 9.2 GiB of memory on the machine in results/real-proofs.txt
cargo test -p vesting_tests --test private real_proofs -- --ignored --nocapture

# the faults; the copy in the work directory is kept
python3 faults.py <work directory> > results/faults.txt
```

## Pins

| Dependency | Version |
|---|---|
| LEZ | [`v0.4.0-rc3`](https://github.com/logos-blockchain/logos-execution-zone/tree/bafd07be0701398f9bc435f73ab05e9a7936c301) |
| Token program | [`lez-programs@3f04cc3`](https://github.com/logos-blockchain/lez-programs/tree/3f04cc37ec5f90358e606b0fe7d28baa6b1dbd7e/programs/token) |
| RISC Zero | 3.0.5 |
| SPEL, for the IDL only | [`d7444f8`](https://github.com/logos-co/spel/tree/d7444f84cabee3371aca3bdee8ebc4c2fc45ebde) |

RISC Zero 3.0.5 is covered by
[GHSA-g6qg-9j8p-7j87](https://github.com/risc0/risc0/security/advisories/GHSA-g6qg-9j8p-7j87), fixed
in 3.1.0. The proofs of this bench are measurements and protect nothing.

## Licence

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option.
