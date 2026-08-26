//! Official QQ endpoint URLs and subscription constants
//! (hermes `constants.py` role).

/// Official WebSocket gateway URL (override for mock-backend tests).
pub const DEFAULT_WS_URL: &str = "wss://api.sgroup.qq.com/websocket";

/// Official token endpoint (override for mock-backend tests).
pub const DEFAULT_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// Official OpenAPI base (override for mock-backend tests).
pub const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";

/// `GROUP_AND_C2C_EVENT` intent bit (C2C + group @-messages).
pub const INTENT_GROUP_AND_C2C: u32 = 1 << 25;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_endpoints_and_intent_bit() {
        assert_eq!(DEFAULT_WS_URL, "wss://api.sgroup.qq.com/websocket");
        assert_eq!(
            DEFAULT_TOKEN_URL,
            "https://bots.qq.com/app/getAppAccessToken"
        );
        assert_eq!(DEFAULT_API_BASE, "https://api.sgroup.qq.com");
        assert_eq!(INTENT_GROUP_AND_C2C, 33_554_432);
    }
}
