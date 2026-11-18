//! Dry-run XMR recovery for a self-ASB protocol-5 swap where Alice punished Bob.
//!
//! This helper never broadcasts. It decrypts both persisted states, checks that
//! the two Monero key shares and amounts agree, and prints the sweep plan. With
//! `--construct` it also builds the sweep transaction against a daemon and can
//! write the signed hex to a file for a separate, explicit broadcast.

use anyhow::{bail, Context, Result};
use dialoguer::Password;
use hmac::Hmac;
use sha2::Sha512;
use std::io::{self, Write};
use swap::database::StateCipher;
use swap::protocol_v5::db;
use swap::protocol_v5::recovery::recover_xmr_from_punished_self_swap;
use swap::seed::Seed;

struct Args {
    bob_state: Option<String>,
    alice_state: Option<String>,
    seed_file: Option<String>,
    mnemonic_file: Option<String>,
    passphrase_file: Option<String>,
    construct: bool,
    destination: Option<String>,
    daemon_url: Option<String>,
    daemon_proxy: Option<String>,
    monero_network: Option<String>,
    sweep_hex_out: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        bob_state: None,
        alice_state: None,
        seed_file: None,
        mnemonic_file: None,
        passphrase_file: None,
        construct: false,
        destination: None,
        daemon_url: None,
        daemon_proxy: None,
        monero_network: None,
        sweep_hex_out: None,
    };

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bob-state" => args.bob_state = iter.next(),
            "--alice-state" => args.alice_state = iter.next(),
            "--seed-file" => args.seed_file = iter.next(),
            "--mnemonic-file" => args.mnemonic_file = iter.next(),
            "--passphrase-file" => args.passphrase_file = iter.next(),
            "--construct" => args.construct = true,
            "--destination" => args.destination = iter.next(),
            "--daemon-url" => args.daemon_url = iter.next(),
            "--daemon-proxy" => args.daemon_proxy = iter.next(),
            "--monero-network" => args.monero_network = iter.next(),
            "--sweep-hex-out" => args.sweep_hex_out = iter.next(),
            other => bail!("unknown argument: {other}"),
        }
    }

    Ok(args)
}

fn seed_from_mnemonic(mnemonic: &str, passphrase: &str) -> Seed {
    let normalized = mnemonic.split_whitespace().collect::<Vec<_>>().join(" ");
    let salt = format!("mnemonic{passphrase}");
    let mut seed = [0u8; 64];
    pbkdf2::pbkdf2::<Hmac<Sha512>>(normalized.as_bytes(), salt.as_bytes(), 2048, &mut seed);
    Seed::from(seed)
}

fn read_seed(args: &Args) -> Result<Seed> {
    if let Some(path) = args.seed_file.as_deref() {
        let encoded = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read seed file {path}"))?;
        return Seed::from_base64(encoded.trim())
            .map_err(|error| anyhow::anyhow!("invalid seed: {error:?}"));
    }

    if let Some(path) = args.mnemonic_file.as_deref() {
        let mnemonic = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read mnemonic file {path}"))?;
        let passphrase = match args.passphrase_file.as_deref() {
            Some(path) => std::fs::read_to_string(path)
                .with_context(|| format!("failed to read passphrase file {path}"))?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
            None => Password::new()
                .with_prompt("Wallet passphrase (leave empty if none)")
                .allow_empty_password(true)
                .interact()?,
        };
        return Ok(seed_from_mnemonic(&mnemonic, &passphrase));
    }

    eprint!("Seed (base64, 64 bytes): ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Seed::from_base64(line.trim()).map_err(|error| anyhow::anyhow!("invalid seed: {error:?}"))
}

fn read_state<T: serde::de::DeserializeOwned>(path: &str, cipher: &StateCipher) -> Result<T> {
    let stored = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read state file {path}"))?;
    let plaintext = cipher
        .decrypt(stored.trim())
        .context("failed to decrypt state (wrong seed or tampered data)")?;
    serde_json::from_slice(&plaintext).context("failed to deserialize state")
}

fn parse_network(value: Option<&str>) -> Result<swap::monero::Network> {
    match value.unwrap_or("stagenet") {
        "mainnet" => Ok(swap::monero::Network::Mainnet),
        "stagenet" => Ok(swap::monero::Network::Stagenet),
        "testnet" => Ok(swap::monero::Network::Testnet),
        other => bail!("unsupported Monero network: {other}"),
    }
}

fn format_xmr(pico: u64) -> String {
    format!(
        "{}.{:012}",
        pico / 1_000_000_000_000,
        pico % 1_000_000_000_000
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let bob_path = args
        .bob_state
        .as_deref()
        .context("--bob-state <path> is required")?;
    let alice_path = args
        .alice_state
        .as_deref()
        .context("--alice-state <path> is required")?;

    let seed = read_seed(&args)?;
    let cipher = StateCipher::from_seed(&seed);

    let bob: db::bob::Bob = read_state(bob_path, &cipher)?;
    let alice: db::alice::Alice = read_state(alice_path, &cipher)?;
    println!("Bob state: {bob}");
    println!("Alice state: {alice}");

    let recovered = recover_xmr_from_punished_self_swap(&bob, &alice)?;
    println!("Monero lock txid: {}", recovered.txid);
    println!(
        "Monero lock amount: {} piconero ({} XMR)",
        recovered.keys.amount_pico,
        format_xmr(recovered.keys.amount_pico)
    );
    println!(
        "Combined public spend key: {}",
        recovered.keys.public_spend_key
    );
    println!(
        "Recovery check: public key matches both private shares; view keys match; amounts match."
    );

    if !args.construct {
        println!("Dry run only. Re-run with --construct to build the sweep transaction.");
        return Ok(());
    }

    let destination = args
        .destination
        .as_deref()
        .context("--destination <XMR address> is required with --construct")?;
    let daemon_url = args
        .daemon_url
        .as_deref()
        .context("--daemon-url <url> is required with --construct")?;
    let network = parse_network(args.monero_network.as_deref())?;
    let destination: swap::monero::Address = destination
        .parse()
        .context("invalid Monero destination address")?;

    let daemon =
        swap::monero::ng::Daemon::connect(daemon_url, args.daemon_proxy.as_deref(), None).await?;
    let sweep = daemon
        .construct_sweep_to_single(
            &recovered.keys.spend_key,
            &recovered.keys.view_key,
            &recovered.txid,
            &destination,
            network,
        )
        .await?;

    println!("Constructed sweep txid: {}", sweep.tx_hash);
    println!("Sweep tx hex length: {} bytes", sweep.tx_hex.len());
    if let Some(path) = args.sweep_hex_out.as_deref() {
        std::fs::write(path, &sweep.tx_hex)
            .with_context(|| format!("failed to write sweep hex to {path}"))?;
        println!("Sweep hex written to {path}");
    }
    println!("Nothing was broadcast. Broadcast only after you verify the txid and amount.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn bip39_seed_matches_known_vector() {
        let seed = seed_from_mnemonic(
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            "TREZOR",
        );
        assert_eq!(
            hex::encode(Sha256::digest(seed.derive_state_encryption_key())),
            "f4fca5be3ebf787197057b228396fcbe88e686ac897e4269af00edd057ee8d8d"
        );
    }
}
