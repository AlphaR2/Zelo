//! `huginn`: runs the Huginn program on the real local transaction, then with the same
//! transaction finalized by synthetic validator sets of chosen sizes, and reports cycles.
//! Every run is also verified natively; the committed public values must match.
//!
//!   cargo run --release -- --synthetic 100,1000,1500 --slow 1000
//!
//! With `--prove`, each run is also proven, the proof verified, and its summary (program
//! key, public values, onchain proof bytes) written to `--out`:
//!
//!   cargo run --release -- --prove compressed
//!   cargo run --release -- --prove groth16      # needs Docker running (gnark wrap)

use {
    alpenglow_core::{fixture, proof::ClaimInput},
    anyhow::Result,
    clap::Parser,
    huginn_prover::{Huginn, Mode, alpenglow_core, tx_input_from_block_file},
    std::path::Path,
};

#[derive(Parser)]
struct Cli {
    /// Block fixture from `ledger-dump` that contains the transaction.
    #[arg(long, default_value = "../../fixtures/local/block_46718.json")]
    block: String,
    /// The transaction's signature, base58 or hex.
    #[arg(
        long,
        default_value = "4F39ig6uWUKU8cAcUYwyCeTasG5U6KgZLoYcbkabhVsbJo3pLWMGkQjsUfUMKDrm7P9HkZrjoFzpRjmAc1YJ8dL"
    )]
    signature: String,
    /// Synthetic fast-path sets of these sizes (~90% signing).
    #[arg(long, value_delimiter = ',')]
    synthetic: Vec<usize>,
    /// Synthetic slow-path sets of these sizes.
    #[arg(long, value_delimiter = ',')]
    slow: Vec<usize>,
    /// Also prove each run: core, compressed or groth16.
    #[arg(long)]
    prove: Option<Mode>,
    /// Run a state claim instead: a `fixtures/local/state_*.json` (from `make_state_fixture`),
    /// certified by synthetic validators. `--depth` descendants form the ancestry and the
    /// witness opens the first `--confirm` of their footers.
    #[arg(long)]
    state: Option<String>,
    #[arg(long, default_value_t = 0)]
    depth: usize,
    #[arg(long, default_value_t = 0)]
    confirm: usize,
    /// Synthetic validators certifying a `--state` run.
    #[arg(long, default_value_t = 21)]
    validators: usize,
    /// Where proof summaries go.
    #[arg(long, default_value = "../../fixtures/proofs")]
    out: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();
    let cli = Cli::parse();
    if let Some(path) = &cli.state {
        let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let fx = fixture::state_fixture(&f, cli.depth, cli.confirm);
        let base = alpenglow_core::proof::FinalityInput {
            validators: vec![],
            shred_version: fx.shred_version,
            slot: fx.certified.0,
            block_id: fx.certified.1,
            final_aggregate: alpenglow_core::proof::Aggregate {
                signature: vec![],
                bitmap: vec![],
            },
            notar_aggregate: None,
        };
        let input = ClaimInput {
            finality: fixture::synthetic(&base, cli.validators),
            ancestry: fx.links,
            tx: None,
            state: Some(fx.witness),
        };
        let huginn = Huginn::from_env().await;
        let e = huginn.execute(&input).await?;
        println!(
            "state claim, {} changed accounts, {} ancestry link(s), {} footer(s) opened: read {} + finality {} + claims {}, total {} cycles",
            input.state.as_ref().unwrap().changes.len(),
            input.ancestry.len(),
            cli.confirm,
            e.read_cycles,
            e.finality_cycles,
            e.inclusion_cycles,
            e.total_cycles
        );
        return Ok(());
    }
    let real = tx_input_from_block_file(&cli.block, &cli.signature)?;

    let path = if real.finality.notar_aggregate.is_none() {
        "fast"
    } else {
        "slow"
    };
    let n = real.finality.validators.len();
    let label = format!(
        "real, {n} validator{}, {path}",
        if n == 1 { "" } else { "s" }
    );
    let mut runs = vec![(label, real.clone())];
    for &n in &cli.synthetic {
        runs.push((
            format!("{n} validators, fast"),
            ClaimInput {
                finality: fixture::synthetic(&real.finality, n),
                ..real.clone()
            },
        ));
    }
    for &n in &cli.slow {
        runs.push((
            format!("{n} validators, slow"),
            ClaimInput {
                finality: fixture::synthetic_slow(&real.finality, n),
                ..real.clone()
            },
        ));
    }

    let huginn = Huginn::from_env().await;
    println!(
        "{:<28} {:>14} {:>14} {:>14}",
        "input", "finality", "inclusion", "total cycles"
    );
    for (name, input) in runs {
        let e = huginn.execute(&input).await?;
        println!(
            "{name:<28} {:>14} {:>14} {:>14}",
            e.finality_cycles, e.inclusion_cycles, e.total_cycles
        );
        let Some(mode) = cli.prove else { continue };

        let proven = huginn.prove(&input, mode).await?;
        // One file name per run: "1000 validators, fast" -> "1000-validators-fast".
        let slug = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let dir = Path::new(&cli.out);
        std::fs::create_dir_all(dir)?;
        let bin = dir.join(format!("{slug}.{}.bin", mode.as_str()));
        proven.proof.save(&bin)?;
        let summary = dir.join(format!("{slug}.{}.json", mode.as_str()));
        std::fs::write(
            &summary,
            serde_json::to_string_pretty(&proven.summary(&name))? + "\n",
        )?;
        println!(
            "  {} proof: setup {:.1}s, prove {:.1}s, verify {:.2}s -> {}",
            mode.as_str(),
            proven.setup_secs,
            proven.prove_secs,
            proven.verify_secs,
            summary.display()
        );
    }
    Ok(())
}
