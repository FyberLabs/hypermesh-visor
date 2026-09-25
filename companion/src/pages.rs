//! Pages the site already publishes. Paths come from FyberLabs/hypermesh
//! (`lib/portal.ts`, `components/payments.tsx`, `components/products.tsx`).

pub const SITE_ORIGIN: &str = "https://hyperme.sh";

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
        assert_eq!(SITE_ORIGIN, "https://hyperme.sh");
        assert_eq!(PORTAL_LOGIN_URL, "https://portal.test.hyperme.sh/login");
        assert_eq!(API_BASE, "https://api.test.hyperme.sh");
        assert_eq!(CHAT_BASE, "https://chat.test.hyperme.sh");
    }
}
