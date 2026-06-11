//! Themes swap the canned vocabulary pools the value generators draw from, so
//! a `status` column reads `shipped`/`refunded` under `ecommerce` but
//! `online`/`degraded` under `iot`. A theme only changes *flavor* — the
//! inference engine and wire types are identical across themes.

#[derive(Debug)]
pub struct ThemeData {
    pub name: &'static str,
    /// Drawn for `status`/`state` columns.
    pub statuses: &'static [&'static str],
    /// Drawn for `type`/`kind`/`category` columns.
    pub kinds: &'static [&'static str],
    /// Blended into lorem text, titles, and slugs to give them a domain feel.
    pub nouns: &'static [&'static str],
}

pub static GENERIC: ThemeData = ThemeData {
    name: "generic",
    statuses: &[
        "active",
        "pending",
        "inactive",
        "archived",
        "suspended",
        "expired",
    ],
    kinds: &[
        "standard",
        "premium",
        "basic",
        "trial",
        "enterprise",
        "legacy",
    ],
    nouns: &[
        "widget", "record", "entry", "item", "node", "object", "unit", "batch",
    ],
};

pub static ECOMMERCE: ThemeData = ThemeData {
    name: "ecommerce",
    statuses: &[
        "pending",
        "paid",
        "shipped",
        "delivered",
        "refunded",
        "cancelled",
        "returned",
    ],
    kinds: &["physical", "digital", "subscription", "bundle", "giftcard"],
    nouns: &[
        "order",
        "cart",
        "sku",
        "shipment",
        "invoice",
        "coupon",
        "catalog",
        "checkout",
        "wishlist",
        "refund",
        "warehouse",
        "pallet",
        "voucher",
        "fulfillment",
    ],
};

pub static FINANCE: ThemeData = ThemeData {
    name: "finance",
    statuses: &[
        "pending", "cleared", "settled", "declined", "disputed", "reversed",
    ],
    kinds: &["checking", "savings", "credit", "loan", "brokerage"],
    nouns: &[
        "ledger",
        "account",
        "transaction",
        "balance",
        "statement",
        "dividend",
        "portfolio",
        "invoice",
        "accrual",
        "liability",
        "remittance",
        "escrow",
    ],
};

pub static IOT: ThemeData = ThemeData {
    name: "iot",
    statuses: &[
        "online",
        "offline",
        "degraded",
        "provisioning",
        "faulted",
        "sleeping",
    ],
    kinds: &["sensor", "actuator", "gateway", "controller", "beacon"],
    nouns: &[
        "telemetry",
        "firmware",
        "sensor",
        "gateway",
        "payload",
        "heartbeat",
        "uplink",
        "threshold",
        "reading",
        "device",
        "calibration",
        "downlink",
    ],
};

pub static USERS: ThemeData = ThemeData {
    name: "users",
    statuses: &["active", "invited", "suspended", "deactivated", "pending"],
    kinds: &["admin", "member", "guest", "owner", "moderator"],
    nouns: &[
        "profile",
        "session",
        "avatar",
        "handle",
        "follower",
        "notification",
        "preference",
        "badge",
        "invite",
        "mention",
    ],
};

static ALL: &[&ThemeData] = &[&GENERIC, &ECOMMERCE, &FINANCE, &IOT, &USERS];

/// Look up a theme by name (case-insensitive). `None` if unknown.
pub fn by_name(name: &str) -> Option<&'static ThemeData> {
    let n = name.to_ascii_lowercase();
    ALL.iter().copied().find(|t| t.name == n)
}

/// All theme names, for CLI help and error messages.
pub fn names() -> Vec<&'static str> {
    ALL.iter().map(|t| t.name).collect()
}
