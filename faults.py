#!/usr/bin/env python3
"""Puts single faults into a copy of the bench and reports which test fails for each.

    python3 faults.py <work directory> > results/faults.txt

The bench is copied into the work directory, which is kept, so that a second run builds faster.
"""
import os, re, shutil, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORK = Path(sys.argv[1]).resolve()
PROGRAM, CORE = "program/src/lib.rs", "core/src/lib.rs"
SEEDS, ACCRUAL = "core/src/seeds.rs", "core/src/accrual.rs"


def check(condition, code):
    return f'            assert!(\n                {condition},\n                "{{}}",\n                error::{code}\n            );\n'


def short(condition, code):
    return f'assert!({condition}, "{{}}", error::{code});'


CANCELLED = check("schedule.cancelled_at.is_none()", "ALREADY_CANCELLED")
BENEFICIARY = short("sender == schedule.beneficiary", "NOT_BENEFICIARY")
CANCELABLE = short("schedule.cancelable", "NOT_CANCELABLE")
WINDOW = ".try_timestamp_window(until.saturating_sub(MAX_CANCEL_LEAD)..until)\n                .expect(error::UNEXPECTED);"

# name, file, text, replacement, which occurrence
FAULTS = [
    ("a person need not authorise", PROGRAM, short("input.is_authorized", "NOT_AUTHORIZED"), "", 1),
    ("a schedule obeys another program", PROGRAM, "input.from_own_program()", "input.from.is_some()", 1),
    ("a schedule is written without its seed", PROGRAM, "input.is_authorized && input.pre_state.is_empty()", "input.pre_state.is_empty()", 1),
    ("a schedule is written over another", PROGRAM, "input.is_authorized && input.pre_state.is_empty()", "input.is_authorized", 1),
    ("the creator may act under another address", PROGRAM, short("sender == creator", "NOT_CREATOR"), "", 1),
    ("anyone claims", PROGRAM, BENEFICIARY, "", 1),
    ("anyone transfers a position", PROGRAM, BENEFICIARY, "", 2),
    ("anyone cancels", PROGRAM, check("sender == schedule.cancel_authority", "NOT_CANCEL_AUTHORITY"), "", 1),
    ("anyone signals", PROGRAM, check("sender == schedule.milestone_authority", "NOT_MILESTONE_AUTHORITY"), "", 1),
    ("anyone converts", PROGRAM, short("sender == schedule.creator", "NOT_CREATOR"), "", 1),
    ("the cancel authority converts too", PROGRAM, "sender == schedule.creator,", "sender == schedule.creator || sender == schedule.cancel_authority,", 1),
    ("a zero claim", PROGRAM, short("amount > 0", "ZERO_AMOUNT"), "", 1),
    ("a claim above what vested", PROGRAM, "amount <= schedule.claimable(as_of)", "amount <= u128::MAX", 1),
    ("a claim is not recorded", PROGRAM, "schedule.claimed += amount;", "", 1),
    ("no window on a claim", PROGRAM, "response.timestamp_window(as_of..)", "response", 1),
    ("a window on milestone claims", PROGRAM, "if schedule.shape.is_time_based() {", "if true {", 1),
    ("no window on the claim of a cliff", ACCRUAL, "!matches!(self, Self::Milestones { .. })", "matches!(self, Self::Linear { .. })", 1),
    ("no window on a cancel", PROGRAM, WINDOW, ";", 1),
    ("no limit ahead on a cancel", PROGRAM, WINDOW, ".timestamp_window(..until);", 1),
    ("a limit of two hours on a cancel", CORE, "3_600_000", "7_200_000", 1),
    ("a backdated cancel", PROGRAM, WINDOW, ".timestamp_window(until.saturating_sub(MAX_CANCEL_LEAD)..);", 1),
    ("a refund ignores what was claimed", PROGRAM, "schedule.vested(until).max(schedule.claimed)", "schedule.vested(until)", 1),
    ("a second cancel", PROGRAM, CANCELLED, "", 1),
    ("conversion after a cancel", PROGRAM, CANCELLED, "", 2),
    ("a signal after a cancel", PROGRAM, CANCELLED, "", 3),
    ("cancelling a non-cancelable schedule", PROGRAM, CANCELABLE, "", 1),
    ("converting twice", PROGRAM, CANCELABLE, "", 2),
    ("signalling twice", PROGRAM, short("*signalled & bit == 0", "ALREADY_SIGNALLED"), "", 1),
    ("transferring a fixed position", PROGRAM, short("schedule.transferable", "NOT_TRANSFERABLE"), "", 1),
    ("the batch number stays", PROGRAM, "state.next_batch += 1;", "", 1),
    ("an empty batch", PROGRAM, short("!schedules.is_empty()", "BAD_SCHEDULE"), "", 1),
    ("a malformed shape", PROGRAM, "spec.total > 0 && spec.shape.is_valid(spec.total)", "spec.total > 0", 1),
    ("a zero total", PROGRAM, "spec.total > 0 && spec.shape.is_valid(spec.total)", "spec.shape.is_valid(spec.total)", 1),
    ("a cliff at the start refused", ACCRUAL, "start <= cliff", "start < cliff", 1),
    ("a batch total that wraps", PROGRAM, "sum.checked_add(spec.total)", "Some(sum.wrapping_add(spec.total))", 1),
    ("a tranche sum that wraps", ACCRUAL, "sum.checked_add(*amount)", "Some(sum.wrapping_add(*amount))", 1),
    ("no limit on tranches", ACCRUAL, "&& tranches.len() <= MAX_TRANCHES", "", 1),
    ("a refund is not paid", PROGRAM, "if refunded == 0 {", "if true {", 1),
    ("a refund to the beneficiary", PROGRAM, "pay(input, &schedule, schedule.source, refunded)", "pay(input, &schedule, schedule.beneficiary, refunded)", 1),
    ("an escrow one token short", PROGRAM, "                amount: total,\n", "                amount: total - 1,\n", 1),
    ("every batch pays from the first escrow", PROGRAM, "seeds::escrow(schedule.creator, schedule.batch)", "seeds::escrow(schedule.creator, 0)", 1),
    ("addresses ignore the creator", SEEDS, "bytes[32..64].copy_from_slice(creator.as_ref());", "let _ = creator;", 1),
    ("a creation announces the wrong batch", PROGRAM, "            batch,\n            spec,\n        }\n        .to_program_event(),", "            batch: 0,\n            spec,\n        }\n        .to_program_event(),", 1),
    ("a signal announces the wrong milestone", PROGRAM, "                    index,\n                    amount,", "                    index: 0,\n                    amount,", 1),
    ("a conversion without its event", PROGRAM, "Response::set_state(encode(&schedule)).event(\n                MadeNonCancelable {\n                    schedule: input.receiver.account_id,\n                }\n                .to_program_event(),\n            )", "{ let _ = MadeNonCancelable { schedule: input.receiver.account_id }; Response::set_state(encode(&schedule)) }", 1),
    ("a transfer without its event", PROGRAM, "Response::set_state(encode(&schedule)).event(\n                BeneficiaryTransferred {\n                    schedule: input.receiver.account_id,\n                    old: sender,\n                    new: to,\n                }\n                .to_program_event(),\n            )", "{ let _ = BeneficiaryTransferred { schedule: input.receiver.account_id, old: sender, new: to }; Response::set_state(encode(&schedule)) }", 1),
]


def put(text, old, new, which):
    at = -1
    for _ in range(which):
        at = text.index(old, at + 1)
    return text[:at] + new + text[at + len(old):]


def test():
    run = subprocess.run(
        ["cargo", "test", "-p", "vesting_core", "-p", "vesting_program", "-p", "vesting_tests", "--no-fail-fast"],
        capture_output=True, text=True, cwd=WORK, env=dict(os.environ, RISC0_DEV_MODE="1"),
    )
    out = run.stdout + run.stderr
    failed = re.findall(r"^test (\S+)(?: - should panic)? \.\.\. FAILED", out, re.M)
    return failed, sum(map(int, re.findall(r"(\d+) passed", out)))


shutil.copytree(HERE, WORK, ignore=shutil.ignore_patterns("target", ".git", "results"), dirs_exist_ok=True)
for name, file, old, new, which in FAULTS:  # every fault must apply
    assert put((HERE / file).read_text(), old, new, which) != (HERE / file).read_text(), name
failed, passed = test()
assert not failed, failed
print(f"{len(FAULTS)} faults; without one, {passed} tests pass", flush=True)
missed = 0
for name, file, old, new, which in FAULTS:
    source = (HERE / file).read_text()
    (WORK / file).write_text(put(source, old, new, which))
    failed, passed = test()
    (WORK / file).write_text(source)
    if failed:
        print(f"caught  {name}: {', '.join(sorted(set(failed)))}", flush=True)
    else:
        missed += 1
        print(f"{'MISSED' if passed else 'DOES NOT BUILD'}  {name}", flush=True)
print(f"{len(FAULTS) - missed} of {len(FAULTS)} caught")
