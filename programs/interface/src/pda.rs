//! Program-derived addresses (PDAs): addresses owned by a program, derived from seeds.

use {
    pinocchio::{
        AccountView, Address, ProgramResult,
        cpi::Signer,
        error::ProgramError,
        sysvars::{Sysvar, rent::Rent},
    },
    pinocchio_system::instructions::{Allocate, Assign, Transfer},
};

/// Checks that `key` is the canonical PDA for `seeds` (the one `find_program_address` returns)
/// and returns its bump.
///
/// A PDA has many valid bumps. Accepting any of them would give one set of seeds several
/// addresses: for example two "filled" markers for one order, so it could be filled twice.
pub fn check_canonical(
    seeds: &[&[u8]],
    program_id: &Address,
    key: &Address,
) -> Result<u8, ProgramError> {
    let (address, bump) = Address::find_program_address(seeds, program_id);
    if address != *key {
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(bump)
}

/// Creates a PDA owned by `owner` with `space` bytes, even if someone already sent it lamports.
///
/// The system program's `CreateAccount` fails on an address that holds lamports, so anyone
/// could block it by sending some first. Instead: top the balance up to rent exemption, then
/// allocate and assign, both signed by the PDA.
pub fn create_pda(
    pda: &AccountView,
    payer: &AccountView,
    owner: &Address,
    space: usize,
    signer: Signer,
) -> ProgramResult {
    let needed = Rent::get()?
        .try_minimum_balance(space)?
        .saturating_sub(pda.lamports());
    if needed > 0 {
        Transfer {
            from: payer,
            to: pda,
            lamports: needed,
        }
        .invoke()?;
    }
    let signers = core::slice::from_ref(&signer);
    Allocate {
        account: pda,
        space: space as u64,
    }
    .invoke_signed(signers)?;
    Assign {
        account: pda,
        owner,
    }
    .invoke_signed(signers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next valid (non-canonical) bump below the canonical one.
    fn other_bump(seeds: &[&[u8]], program: &Address) -> Option<Address> {
        let (_, canonical) = Address::find_program_address(seeds, program);
        (0..canonical).rev().find_map(|b| {
            let bump = [b];
            let mut s = seeds.to_vec();
            s.push(&bump);
            Address::create_program_address(&s, program).ok()
        })
    }

    #[test]
    fn only_the_canonical_address_is_accepted() {
        let program = Address::new_from_array([1; 32]);
        let id = [7u8; 32];
        let seeds: &[&[u8]] = &[b"filled", &id];
        let (canonical, bump) = Address::find_program_address(seeds, &program);
        assert_eq!(check_canonical(seeds, &program, &canonical), Ok(bump));
        let odd = other_bump(seeds, &program).expect("another valid bump exists");
        assert_ne!(odd, canonical);
        assert_eq!(
            check_canonical(seeds, &program, &odd),
            Err(ProgramError::InvalidSeeds)
        );
    }
}
