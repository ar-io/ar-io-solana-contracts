//! Metaplex Core CPI helpers — UpdatePluginV1 for the Attributes plugin.
//!
//! Hand-rolled (no `mpl-core` crate dependency) because the SBF build chain
//! shipped with Solana 2.1.0 uses Cargo 1.79, which can't parse manifests
//! that depend on `edition2024`. The mpl-core crate's transitive deps drift
//! into edition2024 in newer versions and pinning to old versions risks
//! ABI drift. The wire format is stable + small.
//!
//! Wire format reference (kinobi-generated `@metaplex-foundation/mpl-core`,
//! validated byte-for-byte offline):
//!
//! UpdatePluginV1 instruction data:
//!   - discriminator: u8 = 6
//!   - plugin: Plugin (enum, Borsh)
//!     - variant: u8 (Attributes = 6)
//!     - body for Attributes: AttributeList { attribute_list: Vec<Attribute> }
//!     - Attribute: { key: String, value: String }
//!
//! UpdatePluginV1 account order:
//!   0. asset           (writable)
//!   1. collection      (writable, optional → MPL Core program id when None)
//!   2. payer           (writable, signer)
//!   3. authority       (signer, optional → MPL Core program id when None)
//!   4. system_program
//!   5. log_wrapper     (optional → MPL Core program id when None)
//!
//! Authority semantics: ANTs mint with the Attributes plugin's
//! `BasePluginAuthority = Owner`, so only the current asset owner can sign
//! UpdatePluginV1. Sprint 3 / ADR-016 reshape: this CPI lives in `ario-ant`
//! (the ANT program). `ario-arns` and `ario-core` are MPL-agnostic.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
};

use crate::MPL_CORE_PROGRAM_ID;

/// On-chain trait keys, kept identical to the migration mint side
/// (`migration/import/src/phases/phase2-ants.ts`) and the SDK
/// (`sdk/src/solana/metadata.ts`). Marketplaces and DAS indexers query
/// these by exact string match.
pub const TRAIT_KEY_ARNS_NAME: &str = "ArNS Name";
pub const TRAIT_KEY_TYPE: &str = "Type";
pub const TRAIT_KEY_UNDERNAME_LIMIT: &str = "Undername Limit";

/// Trait key for the per-asset ANT program override (ADR-016 / BD-100).
/// Set at mint time (CreateV1) by `migration/import` and the SDK's
/// `spawnSolanaANT`. The canonical `ario_ant` program is used when this
/// attribute is absent; otherwise the value (a base58-encoded program
/// address) names the program that ario-arns / ario-core / ario-gar
/// should treat as the ANT program for this specific asset.
///
/// `sync_attributes` preserves this trait across the whole-list replace
/// performed by UpdatePluginV1 — dropping it would silently break
/// resolution for non-canonical ANTs.
pub const TRAIT_KEY_ANT_PROGRAM: &str = "ANT Program";

/// MPL Core UpdatePluginV1 instruction discriminator (single byte, kinobi-generated).
const UPDATE_PLUGIN_V1_DISCRIMINATOR: u8 = 6;

/// MPL Core Plugin enum: Attributes variant index.
const PLUGIN_VARIANT_ATTRIBUTES: u8 = 6;

/// One on-chain attribute entry. Owned Strings because the Borsh wire format
/// prefixes each with a u32 length and we need to compute total size.
pub struct AttributeKv {
    pub key: String,
    pub value: String,
}

/// Stringified `PurchaseType` for the on-chain `Type` trait. Kept in
/// lock-step with the strings the migration import writes
/// (`migration/import/src/phases/phase2-ants.ts`) — same wire values
/// the AO snapshot's `record.type` uses (`"lease"` / `"permabuy"`).
pub fn purchase_type_str(t: ario_arns::state::PurchaseType) -> &'static str {
    match t {
        ario_arns::state::PurchaseType::Lease => "lease",
        ario_arns::state::PurchaseType::Permabuy => "permabuy",
    }
}

/// Build the Attributes plugin payload for an `ArnsRecord`, preserving
/// any existing `ANT Program` value the asset already carries.
///
/// `existing_ant_program` is the `ANT Program` attribute on the asset
/// before this update. UpdatePluginV1 replaces the entire attribute list,
/// so anything not re-emitted here is wiped — including the asset-side
/// program override that ADR-016 / BD-100 rely on for resolution.
pub fn build_attribute_list(
    record: &ario_arns::state::ArnsRecord,
    existing_ant_program: Option<String>,
) -> Vec<AttributeKv> {
    let mut attrs = vec![
        AttributeKv {
            key: TRAIT_KEY_ARNS_NAME.into(),
            value: record.name.clone(),
        },
        AttributeKv {
            key: TRAIT_KEY_TYPE.into(),
            value: purchase_type_str(record.purchase_type).into(),
        },
        AttributeKv {
            key: TRAIT_KEY_UNDERNAME_LIMIT.into(),
            value: record.undername_limit.to_string(),
        },
    ];
    if let Some(program) = existing_ant_program {
        attrs.push(AttributeKv {
            key: TRAIT_KEY_ANT_PROGRAM.into(),
            value: program,
        });
    }
    attrs
}

/// Build the raw instruction data for UpdatePluginV1 with an Attributes plugin
/// containing the given `attributes` (which may be empty to clear all traits).
pub fn encode_update_attributes_ix_data(attributes: &[AttributeKv]) -> Vec<u8> {
    // Pre-size: 1 (disc) + 1 (plugin variant) + 4 (attr_list len) + sum(per-attr).
    let per_attr: usize = attributes
        .iter()
        .map(|a| 4 + a.key.len() + 4 + a.value.len())
        .sum();
    let mut buf = Vec::with_capacity(1 + 1 + 4 + per_attr);

    buf.push(UPDATE_PLUGIN_V1_DISCRIMINATOR);
    buf.push(PLUGIN_VARIANT_ATTRIBUTES);

    // Borsh: Vec<T> = u32(LE) length + items
    buf.extend_from_slice(&(attributes.len() as u32).to_le_bytes());
    for attr in attributes {
        // Borsh: String = u32(LE) length + bytes
        buf.extend_from_slice(&(attr.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(attr.key.as_bytes());
        buf.extend_from_slice(&(attr.value.len() as u32).to_le_bytes());
        buf.extend_from_slice(attr.value.as_bytes());
    }
    buf
}

/// Read a single Attributes-plugin trait value from a Metaplex Core asset's
/// raw account data. Returns `None` if the asset has no Attributes plugin
/// or the requested key is absent.
///
/// Used by `sync_attributes` to preserve the `ANT Program` trait across
/// UpdatePluginV1's whole-list replace.
///
/// Layout (validated byte-for-byte against kinobi-generated mpl-core
/// fixtures + the migration import golden vectors):
///   - 1     AssetV1 discriminator (= 1)
///   - 32    owner: Pubkey
///   - 33    update_authority: UpdateAuthority enum (variant byte + body)
///       - None → 1 byte
///       - Address(Pubkey) → 1 + 32
///       - Collection(Pubkey) → 1 + 32
///   - …     name: String (4 + N)
///   - …     uri: String (4 + N)
///   - …     seq: Option<u64> (1 + [8])
///   - …     plugin_header: PluginHeaderV1 ([key:u8][plugin_registry_offset:u32])
///   - …     plugin entries (each prefixed with their header offset)
///
/// MPL Core appends each plugin's body lazily and the registry indexes them,
/// so the simplest correct path for "fish out one Attribute key" is to scan
/// for the Attributes plugin variant byte (= 6) at the registry offset and
/// then walk the AttributeList. Rather than pull the registry (which adds
/// fragility — we'd be re-implementing all the plugin index logic), we scan
/// the trailing bytes for the plugin signature pattern. A miss returns None.
pub fn read_existing_attribute(asset_data: &[u8], key: &str) -> Option<String> {
    // Find the Attributes plugin signature: variant byte 6 followed by a
    // u32-len attribute_list whose entries match (key, value) shape.
    //
    // We scan from offset 1 (skip the AssetV1 disc byte) for any byte == 6
    // that is followed by a plausible vec<Attribute>. False positives are
    // possible but checked: the next 4 bytes must be a vec length that
    // doesn't run off the buffer, and each [key_len, key, val_len, val]
    // pair must read valid UTF-8 within bounds.
    let key_bytes = key.as_bytes();
    let mut i = 1usize;
    while i + 5 <= asset_data.len() {
        if asset_data[i] == PLUGIN_VARIANT_ATTRIBUTES {
            if let Some(found) = try_parse_attributes_at(asset_data, i + 1, key_bytes) {
                return found;
            }
        }
        i += 1;
    }
    None
}

fn try_parse_attributes_at(data: &[u8], mut p: usize, want_key: &[u8]) -> Option<Option<String>> {
    if p + 4 > data.len() {
        return None;
    }
    let count = u32::from_le_bytes(data[p..p + 4].try_into().ok()?) as usize;
    p += 4;
    // Sanity cap: an asset with more than 64 attribute traits is not a real ANT;
    // also bounds the false-positive cost of scanning.
    if count > 64 {
        return None;
    }
    let mut found: Option<String> = None;
    for _ in 0..count {
        if p + 4 > data.len() {
            return None;
        }
        let kl = u32::from_le_bytes(data[p..p + 4].try_into().ok()?) as usize;
        p += 4;
        if kl > 256 || p + kl > data.len() {
            return None;
        }
        let k = &data[p..p + kl];
        p += kl;
        if p + 4 > data.len() {
            return None;
        }
        let vl = u32::from_le_bytes(data[p..p + 4].try_into().ok()?) as usize;
        p += 4;
        if vl > 1024 || p + vl > data.len() {
            return None;
        }
        let v = &data[p..p + vl];
        p += vl;
        if k == want_key {
            // Some(Some(value)) = found, Some(None) = matched-but-not-utf8 (still
            // a positive parse). The outer wrapping Some signals "this looked
            // like a real attributes plugin"; the inner is the actual value.
            found = Some(std::str::from_utf8(v).ok()?.to_string());
        }
    }
    // If we successfully parsed `count` entries, we're confident this was the
    // attributes plugin even if the requested key wasn't present.
    Some(found)
}

/// Invoke MPL Core's UpdatePluginV1 to overwrite the asset's Attributes plugin.
///
/// `authority` must be the asset's current owner (we mint with plugin
/// authority = `Owner`). Caller of `sync_attributes` is the verified asset
/// holder (Anchor `authority` signer); pass it for both `payer` and
/// `authority` and the CPI succeeds. `mpl_core_program` is pinned to
/// `MPL_CORE_PROGRAM_ID` by an Anchor account constraint upstream.
pub fn update_attributes_plugin<'info>(
    asset: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    attributes: &[AttributeKv],
) -> Result<()> {
    // Optional accounts (collection, log_wrapper) are signaled to MPL Core by
    // passing the MPL Core program id with isWritable=false / isSigner=false —
    // the kinobi convention used by every official Core client.
    let collection_placeholder = mpl_core_program.clone();
    let log_wrapper_placeholder = mpl_core_program.clone();

    let metas = vec![
        AccountMeta::new(asset.key(), false),
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // collection (None)
        AccountMeta::new(payer.key(), true),                   // payer (signer, writable)
        AccountMeta::new_readonly(authority.key(), true),      // authority (signer)
        AccountMeta::new_readonly(system_program.key(), false),
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // log_wrapper (None)
    ];

    let ix = Instruction {
        program_id: MPL_CORE_PROGRAM_ID,
        accounts: metas,
        data: encode_update_attributes_ix_data(attributes),
    };

    invoke(
        &ix,
        &[
            asset.clone(),
            collection_placeholder,
            payer.clone(),
            authority.clone(),
            system_program.clone(),
            log_wrapper_placeholder,
        ],
    )?;
    Ok(())
}

// =========================================================================
// Tests — wire format pinned against the same fixtures the migration import
// package validates offline.
// =========================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    #[test]
    fn empty_attribute_list_encodes_to_six_bytes() {
        // disc(06) + plugin_variant(06) + vec_len(00 00 00 00)
        let data = encode_update_attributes_ix_data(&[]);
        assert_eq!(hex(&data), "060600000000");
    }

    #[test]
    fn three_attributes_match_kinobi_offline_fixture() {
        let attrs = vec![
            AttributeKv {
                key: "ArNS Name".into(),
                value: "testname".into(),
            },
            AttributeKv {
                key: "Type".into(),
                value: "permabuy".into(),
            },
            AttributeKv {
                key: "Undername Limit".into(),
                value: "10".into(),
            },
        ];
        let data = encode_update_attributes_ix_data(&attrs);

        let expected = concat!(
            "0606",
            "03000000",
            "0900000041724e53204e616d65",
            "08000000746573746e616d65",
            "0400000054797065",
            "080000007065726d616275790f000000556e6465726e616d65204c696d6974",
            "020000003130",
        );
        assert_eq!(hex(&data), expected);
    }

    #[test]
    fn single_attribute_round_trips_lengths() {
        let attrs = vec![AttributeKv {
            key: "k".into(),
            value: "v".into(),
        }];
        let data = encode_update_attributes_ix_data(&attrs);
        assert_eq!(
            hex(&data),
            "06060100000001000000".to_string() + "6b" + "01000000" + "76"
        );
    }

    fn make_record(name: &str) -> ario_arns::state::ArnsRecord {
        ario_arns::state::ArnsRecord {
            name_hash: [0u8; 32],
            owner: anchor_lang::prelude::Pubkey::default(),
            ant: anchor_lang::prelude::Pubkey::default(),
            purchase_type: ario_arns::state::PurchaseType::Permabuy,
            start_timestamp: 0,
            end_timestamp: None,
            undername_limit: 10,
            purchase_price: 1_000_000,
            bump: 0,
            version: ario_arns::state::SchemaVersion::new(1, 0, 0),
            name: name.to_string(),
        }
    }

    #[test]
    fn build_attribute_list_omits_ant_program_when_absent() {
        // Default: no asset-side override → 3-entry payload (canonical ANT).
        let record = make_record("ardrive");
        let attrs = build_attribute_list(&record, None);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].key, TRAIT_KEY_ARNS_NAME);
        assert_eq!(attrs[1].key, TRAIT_KEY_TYPE);
        assert_eq!(attrs[2].key, TRAIT_KEY_UNDERNAME_LIMIT);
    }

    #[test]
    fn build_attribute_list_appends_ant_program_when_present() {
        // ADR-016 / BD-100: forward the asset-side override so the trait
        // survives the next UpdatePluginV1 (which is a whole-list replace).
        let record = make_record("ardrive");
        let attrs = build_attribute_list(
            &record,
            Some("AntPgm111111111111111111111111111111111111".into()),
        );
        assert_eq!(attrs.len(), 4);
        assert_eq!(attrs[3].key, TRAIT_KEY_ANT_PROGRAM);
        assert_eq!(attrs[3].value, "AntPgm111111111111111111111111111111111111");
    }

    #[test]
    fn read_existing_attribute_finds_value_after_synthetic_header() {
        // Synthetic asset blob: 1 byte AssetV1 disc + arbitrary header bytes
        // + Attributes plugin variant byte + 1-attribute vec.
        let mut blob = vec![1u8]; // AssetV1 disc
        blob.extend_from_slice(&[0u8; 32]); // owner
        blob.push(0); // UpdateAuthority::None
        blob.extend_from_slice(&[1u8, 0, 0, 0, b'n']); // name="n"
        blob.extend_from_slice(&[1u8, 0, 0, 0, b'u']); // uri="u"
        blob.push(0); // seq=None
        blob.push(PLUGIN_VARIANT_ATTRIBUTES); // <- the signature scan finds this
        blob.extend_from_slice(&1u32.to_le_bytes()); // 1 attribute
        blob.extend_from_slice(&11u32.to_le_bytes());
        blob.extend_from_slice(b"ANT Program");
        blob.extend_from_slice(&3u32.to_le_bytes());
        blob.extend_from_slice(b"abc");

        assert_eq!(
            read_existing_attribute(&blob, "ANT Program"),
            Some("abc".into())
        );
        assert_eq!(read_existing_attribute(&blob, "Other"), None);
    }
}
