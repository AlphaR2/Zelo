//! Reads `agave-ledger-tool accounts --include-sysvars` output (host side only, feature
//! `fixture`), so S1's checks and the state-proof fixtures can start from a real ledger.
//!
//! The tool prints balances as f64 SOL, which cannot hold a balance of hundreds of millions of
//! SOL to the lamport (a local chain's faucet). [`Dump::parse`] marks such an account
//! `imprecise`; the caller recovers its lamports by matching a known checksum
//! ([`recover_lamports`]).

use crate::bank::{account_lt_hash, LtHash};

pub const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn b58_decode(s: &str) -> Vec<u8> {
    let mut n: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = BASE58.iter().position(|&a| a == c).expect("base58") as u32;
        for b in n.iter_mut() {
            carry += (*b as u32) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            n.push(carry as u8);
            carry >>= 8;
        }
    }
    n.extend(s.bytes().take_while(|&c| c == b'1').map(|_| 0));
    n.reverse();
    n
}

pub fn key(s: &str) -> [u8; 32] {
    b58_decode(s).try_into().expect("32-byte key")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub pubkey: [u8; 32],
    pub lamports: u64,
    pub data: Vec<u8>,
    pub executable: bool,
    pub owner: [u8; 32],
    /// The printed balance cannot be trusted to the lamport.
    pub imprecise: bool,
}

pub struct Dump {
    pub accounts: Vec<Parsed>,
}

fn lamports(sol: &str) -> u64 {
    let (whole, frac) = sol.split_once('.').unwrap_or((sol, ""));
    let mut frac = frac.to_string();
    while frac.len() < 9 {
        frac.push('0');
    }
    let w: u64 = whole.parse().unwrap_or_else(|_| panic!("balance {sol:?}"));
    let f: u64 = frac[..9].parse().unwrap_or_else(|_| panic!("balance {sol:?}"));
    w * 1_000_000_000 + f
}

/// One hex-dump line: `0000:   73 6f 6c 61  6e ...   ascii`. The offset column is as wide as
/// the data needs (`0000:` or `000000:`), then 3 blanks; byte i sits at `base + 3 i + i / 4`.
fn dump_line(line: &str, out: &mut Vec<u8>, want: usize) {
    let b = line.as_bytes();
    let base = line.find(':').expect("offset column") + 4;
    for i in 0..16 {
        if out.len() == want {
            return;
        }
        let at = base + 3 * i + i / 4;
        let byte = b
            .get(at..at + 2)
            .and_then(|x| std::str::from_utf8(x).ok())
            .and_then(|x| u8::from_str_radix(x, 16).ok());
        out.push(byte.unwrap_or_else(|| panic!("hex dump line {line:?}, byte {i}")));
    }
}

impl Dump {
    pub fn parse(text: &str) -> Dump {
        let mut accounts = Vec::new();
        let mut lines = text.lines().peekable();
        while let Some(l) = lines.next() {
            let Some(pk) = l.strip_prefix("Public Key: ") else { continue };
            let mut get = |p: &str| {
                lines
                    .next()
                    .unwrap()
                    .strip_prefix(p)
                    .unwrap_or_else(|| panic!("expected {p}"))
                    .trim()
                    .to_string()
            };
            let lam = lamports(get("Balance: ").trim_end_matches(" SOL"));
            let owner = get("Owner: ");
            let executable = get("Executable: ") == "true";
            let _rent_epoch = get("Rent Epoch: ");
            // An account with no data has no "Length:" line.
            let len: usize = match lines.peek().and_then(|l| l.strip_prefix("Length: ")) {
                Some(rest) => {
                    let n = rest.split(' ').next().unwrap().parse().unwrap();
                    lines.next();
                    n
                }
                None => 0,
            };
            let mut data = Vec::with_capacity(len);
            while data.len() < len {
                dump_line(lines.next().unwrap(), &mut data, len);
            }
            accounts.push(Parsed {
                pubkey: key(pk),
                lamports: lam,
                data,
                executable,
                owner: key(&owner),
                imprecise: lam >= 1_000_000 * 1_000_000_000 && !lam.is_multiple_of(1_000_000_000),
            });
        }
        Dump { accounts }
    }

    /// The lattice hash of all accounts, with the (at most one) imprecise balance recovered by
    /// searching ±5,000 lamports for the value that reproduces `checksum`. Returns the hash
    /// and the recovered lamports, if there was an imprecise account; `None` if no value
    /// matches (the sum then uses the printed balance and will not match `checksum`).
    pub fn lt_hash(&mut self, checksum: &[u8; 32]) -> (LtHash, Option<u64>) {
        let mut lt = LtHash::IDENTITY;
        let mut big = None;
        for (i, a) in self.accounts.iter().enumerate() {
            if a.imprecise {
                assert!(big.is_none(), "more than one balance too large for f64");
                big = Some(i);
            } else {
                lt.add(&account_lt_hash(a.lamports, &a.data, a.executable, &a.owner, &a.pubkey));
            }
        }
        let Some(i) = big else { return (lt, None) };
        let a = self.accounts[i].clone();
        let found = recover_lamports(&lt, &a, checksum);
        let l = found.unwrap_or(a.lamports);
        self.accounts[i].lamports = l;
        self.accounts[i].imprecise = found.is_none();
        lt.add(&account_lt_hash(l, &a.data, a.executable, &a.owner, &a.pubkey));
        (lt, found)
    }
}

/// The lamports within ±5,000 of the printed value that make `rest` plus this account hash to
/// `checksum`.
pub fn recover_lamports(rest: &LtHash, a: &Parsed, checksum: &[u8; 32]) -> Option<u64> {
    (a.lamports.saturating_sub(5_000)..=a.lamports + 5_000).find(|&l| {
        let mut t = rest.clone();
        t.add(&account_lt_hash(l, &a.data, a.executable, &a.owner, &a.pubkey));
        &t.checksum() == checksum
    })
}
