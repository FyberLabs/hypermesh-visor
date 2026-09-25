//! Pages the site already publishes. Paths come from FyberLabs/hypermesh
//! (`lib/portal.ts`, `components/payments.tsx`, `components/products.tsx`).

pub const SITE_ORIGIN: &str = "https://hyperme.sh";
pub const OAUTH_PATH: &str = "/oauth/desktop";

/// Payments section on the marketing site (`components/payments.tsx`).
pub const BILLING_URL: &str = "https://hyperme.sh/#pricing";

/// "What you can rent" (`components/products.tsx`).
pub const RENT_URL: &str = "https://hyperme.sh/#offers";

/// Portal desk linked from the site (`lib/portal.ts` `PORTAL_DESK_URL`).
pub const DASHBOARD_URL: &str = "https://portal.test.hyperme.sh/dashboard/hypermesh";

/// Portal login linked from the site header (`lib/portal.ts` `PORTAL_LOGIN_URL`).
#[allow(dead_code)]
pub const PORTAL_LOGIN_URL: &str = "https://portal.test.hyperme.sh/login";

pub const API_BASE: &str = "https://api.test.hyperme.sh";
pub const CHAT_BASE: &str = "https://chat.test.hyperme.sh";

/// Public PKCE client already registered for loopback port 3000.
/// Token endpoint of the test-plane realm. The site redirect sends the browser here.
pub const OAUTH_CLIENT_ID: &str = "controlplane-frontend";
pub const TOKEN_URL: &str =
    "https://auth.test.hyperme.sh/realms/controlplane/protocol/openid-connect/token";

/// Matches a redirect URI already allowed on `controlplane-frontend`.
pub const LOOPBACK_REDIRECT: &str = "http://127.0.0.1:3000/callback";

pub fn oauth_start_url(state: &str, code_challenge: &str) -> String {
    format!(
        "{SITE_ORIGIN}{OAUTH_PATH}?redirect_uri={}&state={}&code_challenge={}",
        encode_query(LOOPBACK_REDIRECT),
        encode_query(state),
        encode_query(code_challenge),
    )
}

pub fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_the_site_urls() {
        assert_eq!(BILLING_URL, "https://hyperme.sh/#pricing");
        assert_eq!(RENT_URL, "https://hyperme.sh/#offers");
        assert_eq!(
            DASHBOARD_URL,
            "https://portal.test.hyperme.sh/dashboard/hypermesh"
        );
        assert_eq!(PORTAL_LOGIN_URL, "https://portal.test.hyperme.sh/login");
        assert!(oauth_start_url("st", "ch").starts_with("https://hyperme.sh/oauth/desktop?"));
        assert!(oauth_start_url("st", "ch").contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A3000%2Fcallback"));
    }
}
