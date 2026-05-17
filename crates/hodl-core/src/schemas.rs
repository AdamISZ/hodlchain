//! Doc-only stubs for external (non-hodlcoin) types that show up on
//! our HTTP wire. These structs exist only to give `utoipa` a shape to
//! describe in the OpenAPI spec — they are never instantiated; the
//! real types serialise with the same JSON shape.

use utoipa::ToSchema;

/// `bitcoin::OutPoint` — serialised as `{txid, vout}`. The txid is a
/// 32-byte hash hex-encoded (big-endian display order, per Bitcoin
/// convention).
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct OutPointWire {
    /// 32-byte txid, hex-encoded.
    #[schema(example = "0000000000000000000000000000000000000000000000000000000000000000")]
    pub txid: String,
    /// Output index (vout) within the referenced tx.
    pub vout: u32,
}
