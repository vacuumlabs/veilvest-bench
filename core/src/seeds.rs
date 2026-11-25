//! Program-derived addresses. Distinct prefixes and fixed-length fields keep the spaces apart.

use lee_core::{account::AccountId, program::PdaSeed};
use risc0_zkvm::sha::{Impl, Sha256 as _};

const ESCROW: &[u8; 32] = b"/Veilvest/v1/Escrow/\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
const SCHEDULE: &[u8; 32] = b"/Veilvest/v1/Schedule/\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

fn seed(prefix: &[u8; 32], creator: AccountId, batch: u64, index: u32) -> PdaSeed {
    let mut bytes = [0u8; 80];
    bytes[..32].copy_from_slice(prefix);
    bytes[32..64].copy_from_slice(creator.as_ref());
    bytes[64..72].copy_from_slice(&batch.to_le_bytes());
    bytes[72..76].copy_from_slice(&index.to_le_bytes());
    PdaSeed::new(
        Impl::hash_bytes(&bytes)
            .as_bytes()
            .try_into()
            .expect("a sha256 digest has 32 bytes"),
    )
}

#[must_use]
pub fn escrow(creator: AccountId, batch: u64) -> PdaSeed {
    seed(ESCROW, creator, batch, 0)
}

#[must_use]
pub fn schedule(creator: AccountId, batch: u64, index: u32) -> PdaSeed {
    seed(SCHEDULE, creator, batch, index)
}

#[must_use]
pub fn escrow_id(program: AccountId, creator: AccountId, batch: u64) -> AccountId {
    AccountId::for_public_pda(&program, &escrow(creator, batch))
}

#[must_use]
pub fn schedule_id(program: AccountId, creator: AccountId, batch: u64, index: u32) -> AccountId {
    AccountId::for_public_pda(&program, &schedule(creator, batch, index))
}
