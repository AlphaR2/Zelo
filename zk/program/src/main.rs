//! Huginn's zkVM program. Reads a `ClaimInput` (a finality certificate, the parent links to
//! the block in question, and claims about that block: a transaction in it, its change set), runs exactly the checks the native tests run, and commits the public values
//! in their fixed byte layout. If any check fails the program panics, so no proof exists.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alpenglow_core::proof::ClaimInput;

pub fn main() {
    // Reading the input is measured too: it grows with the witness (a change set's account data).
    println!("cycle-tracker-report-start: read");
    let input: ClaimInput = sp1_zkvm::io::read();
    println!("cycle-tracker-report-end: read");

    // Cycle-tracker markers split the report into the two halves of the statement.
    println!("cycle-tracker-report-start: finality");
    let finality = input.finality.verify().expect("finality");
    println!("cycle-tracker-report-end: finality");

    // Walk the parent links from the certified block to the block the claims are about, then
    // check each claim: find the transaction and its signatures, open the change set.
    println!("cycle-tracker-report-start: inclusion");
    let output = input.verify_block(finality).expect("inclusion");
    println!("cycle-tracker-report-end: inclusion");

    sp1_zkvm::io::commit_slice(&output.encode());
}
