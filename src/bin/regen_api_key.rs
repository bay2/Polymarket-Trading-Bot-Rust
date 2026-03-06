use anyhow::{Context, Result};
use std::str::FromStr;
use std::borrow::Cow;

use alloy::core::sol;
use alloy::dyn_abi::Eip712Domain;
use alloy::primitives::U256;
use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;

use serde_json::Value;

use polymarket_client_sdk::POLYGON;

sol! {
    struct ClobAuth {
        address address;
        string  timestamp;
        uint256 nonce;
        string  message;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args().nth(1).unwrap_or_else(|| "config.json".to_string());
    let config_str = std::fs::read_to_string(&config_path)
        .context(format!("Failed to read {config_path}"))?;
    let config: Value = serde_json::from_str(&config_str)?;

    let private_key = config["polymarket"]["private_key"]
        .as_str()
        .context("Missing private_key in config")?;

    let clob_url = config["polymarket"]["clob_api_url"]
        .as_str()
        .unwrap_or("https://clob.polymarket.com");

    let signer = PrivateKeySigner::from_str(private_key.trim_start_matches("0x"))
        .context("Invalid private key")?
        .with_chain_id(Some(POLYGON));

    let address = signer.address();
    eprintln!("Wallet address: {address}");
    eprintln!("CLOB URL: {clob_url}");

    // Generate L1 auth headers (same as SDK's l1::create_headers)
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let nonce: u32 = 0;

    let auth = ClobAuth {
        address,
        timestamp: timestamp.to_string(),
        nonce: U256::from(nonce),
        message: "This message attests that I control the given wallet".to_owned(),
    };

    let domain = Eip712Domain {
        name: Some(Cow::Borrowed("ClobAuthDomain")),
        version: Some(Cow::Borrowed("1")),
        chain_id: Some(U256::from(POLYGON)),
        ..Eip712Domain::default()
    };

    use alloy::sol_types::SolStruct;
    let hash = auth.eip712_signing_hash(&domain);
    let signature = signer.sign_hash(&hash).await?;

    use alloy::hex::ToHexExt;
    let poly_address = address.encode_hex_with_prefix();
    let poly_signature = signature.to_string();
    let poly_timestamp = timestamp.to_string();
    let poly_nonce = nonce.to_string();

    eprintln!();
    eprintln!("L1 Auth headers generated. Calling derive-api-key...");

    // Try derive first, then create
    let client = reqwest::Client::new();

    let result = client
        .get(format!("{clob_url}/auth/derive-api-key"))
        .header("POLY_ADDRESS", &poly_address)
        .header("POLY_SIGNATURE", &poly_signature)
        .header("POLY_TIMESTAMP", &poly_timestamp)
        .header("POLY_NONCE", &poly_nonce)
        .send()
        .await;

    let response = match result {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("derive-api-key request failed: {e}");
            eprintln!();
            eprintln!("Trying with curl fallback...");
            // Fall back to curl
            let output = std::process::Command::new("curl")
                .args([
                    "-s",
                    "-X", "GET",
                    &format!("{clob_url}/auth/derive-api-key"),
                    "-H", &format!("POLY_ADDRESS: {poly_address}"),
                    "-H", &format!("POLY_SIGNATURE: {poly_signature}"),
                    "-H", &format!("POLY_TIMESTAMP: {poly_timestamp}"),
                    "-H", &format!("POLY_NONCE: {poly_nonce}"),
                ])
                .output()
                .context("Failed to run curl")?;

            let body = String::from_utf8_lossy(&output.stdout);
            eprintln!("Derive response: {body}");

            if body.contains("error") || body.is_empty() {
                eprintln!("Derive failed, trying create...");
                let output = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "-X", "POST",
                        &format!("{clob_url}/auth/api-key"),
                        "-H", &format!("POLY_ADDRESS: {poly_address}"),
                        "-H", &format!("POLY_SIGNATURE: {poly_signature}"),
                        "-H", &format!("POLY_TIMESTAMP: {poly_timestamp}"),
                        "-H", &format!("POLY_NONCE: {poly_nonce}"),
                    ])
                    .output()
                    .context("Failed to run curl")?;
                let body = String::from_utf8_lossy(&output.stdout);
                eprintln!("Create response: {body}");
                let creds: Value = serde_json::from_str(&body)
                    .context("Failed to parse create response")?;
                print_credentials(&creds);
                return Ok(());
            }

            let creds: Value = serde_json::from_str(&body)
                .context("Failed to parse derive response")?;
            print_credentials(&creds);
            return Ok(());
        }
    };

    let status = response.status();
    let body = response.text().await?;
    eprintln!("Derive status: {status}, body: {body}");

    if status.is_success() {
        let creds: Value = serde_json::from_str(&body)?;
        print_credentials(&creds);
        return Ok(());
    }

    // Try create
    eprintln!();
    eprintln!("Derive failed, trying create-api-key...");

    let response = client
        .post(format!("{clob_url}/auth/api-key"))
        .header("POLY_ADDRESS", &poly_address)
        .header("POLY_SIGNATURE", &poly_signature)
        .header("POLY_TIMESTAMP", &poly_timestamp)
        .header("POLY_NONCE", &poly_nonce)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    eprintln!("Create status: {status}, body: {body}");

    let creds: Value = serde_json::from_str(&body)
        .context("Failed to parse create response")?;
    print_credentials(&creds);

    Ok(())
}

fn print_credentials(creds: &Value) {
    eprintln!();
    eprintln!("=== New API Credentials ===");
    if let Some(key) = creds.get("apiKey").and_then(|v| v.as_str()) {
        eprintln!("api_key:        {key}");
    }
    if let Some(secret) = creds.get("secret").and_then(|v| v.as_str()) {
        eprintln!("api_secret:     {secret}");
    }
    if let Some(passphrase) = creds.get("passphrase").and_then(|v| v.as_str()) {
        eprintln!("api_passphrase: {passphrase}");
    }
    eprintln!();
    eprintln!("Update these values in your config.json");
}
