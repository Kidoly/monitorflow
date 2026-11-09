//! `monitorflow-plugin`: makes signing keys and signed plugin files on the
//! publisher's own machine (ADR-0103). Nothing here talks to a server.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use clap::{Parser, Subcommand};
use monitorflow_plugin::keys::PublicKey;
use monitorflow_plugin::package::{self, Package};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

#[derive(Parser)]
#[command(version, about = "Sign MonitorFlow plugins")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Make a signing key. Keep the file on this machine; give out only
    /// the public key it prints.
    Keygen {
        /// Where to write the private key; refused if it exists.
        #[arg(long)]
        out: PathBuf,
    },
    /// Check a manifest and a module and sign them into a plugin file.
    Pack {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        module: PathBuf,
        /// The private key from `keygen`.
        #[arg(long)]
        key: PathBuf,
        /// Defaults to NAME-VERSION.mfplugin.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Show what a plugin file holds and asks for, as administrators see it.
    Inspect { file: PathBuf },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen { out } => keygen(&out),
        Command::Pack {
            manifest,
            module,
            key,
            out,
        } => pack(&manifest, &module, &key, out),
        Command::Inspect { file } => inspect(&file),
    }
}

fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn keygen(out: &Path) -> anyhow::Result<()> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("could not make a key"))?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| anyhow::anyhow!("could not read the new key"))?;
    let key = PublicKey::from_bytes(pair.public_key().as_ref())?;
    let pem = pem::encode(&pem::Pem::new("PRIVATE KEY", pkcs8.as_ref().to_vec()));
    create_private(out)
        .and_then(|mut file| file.write_all(pem.as_bytes()))
        .with_context(|| format!("cannot write {}", out.display()))?;
    println!(
        "Private key written to {} (keep it on this machine).",
        out.display()
    );
    println!();
    println!("Public key, to paste under Administration > Plugins > Publishers:");
    println!("{}", key.to_base64());
    println!();
    println!("The same key in PEM:");
    print!("{}", key.to_pem());
    println!("Fingerprint: {}", key.fingerprint());
    Ok(())
}

fn read_key(path: &Path) -> anyhow::Result<Ed25519KeyPair> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let block = pem::parse(text).context("the key file is not PEM")?;
    if block.tag() != "PRIVATE KEY" {
        bail!("the key file holds a {}, not a PRIVATE KEY", block.tag());
    }
    Ed25519KeyPair::from_pkcs8(block.contents())
        .map_err(|_| anyhow::anyhow!("not an Ed25519 key from monitorflow-plugin keygen"))
}

fn pack(manifest: &Path, module: &Path, key: &Path, out: Option<PathBuf>) -> anyhow::Result<()> {
    let manifest_text = std::fs::read_to_string(manifest)
        .with_context(|| format!("cannot read {}", manifest.display()))?;
    let wasm =
        std::fs::read(module).with_context(|| format!("cannot read {}", module.display()))?;
    let pair = read_key(key)?;
    let package = Package::sign(&manifest_text, &wasm, &pair)?;
    let parsed = monitorflow_plugin::Manifest::parse(&manifest_text)?;
    let out = out.unwrap_or_else(|| format!("{}-{}.mfplugin", parsed.name, parsed.version).into());
    let bytes = package.to_bytes();
    std::fs::write(&out, &bytes).with_context(|| format!("cannot write {}", out.display()))?;
    println!("Wrote {} ({} bytes).", out.display(), bytes.len());
    println!("SHA-256: {}", package::hex(&package::sha256(&bytes)));
    Ok(())
}

fn inspect(file: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let package = Package::parse(&bytes)?;
    let key = package.key()?;
    // Checks the file against its own key: whether an install trusts that
    // key is the server's decision.
    let verified = package.verify(&bytes, &[key])?;
    let manifest = &verified.manifest;
    println!(
        "{} {} ({})",
        manifest.title, manifest.version, manifest.name
    );
    if !manifest.description.is_empty() {
        println!("{}", manifest.description);
    }
    println!();
    for line in manifest.capabilities_in_words() {
        println!("- {line}");
    }
    if !manifest.settings.is_empty() {
        println!();
        println!("Settings:");
        for setting in &manifest.settings {
            let required = if setting.required { ", required" } else { "" };
            println!("- {} ({:?}{required})", setting.label, setting.kind);
        }
    }
    println!();
    let signer = if verified.official {
        "MonitorFlow"
    } else {
        "a publisher"
    };
    println!("Signed by {signer}, key fingerprint {}.", key.fingerprint());
    println!("SHA-256: {}", package::hex(&verified.sha256));
    Ok(())
}
