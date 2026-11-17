//! Sensitive-action triggers (06 §10, D21, research §4.3): deterministic, in the daemon,
//! independent of the model.
//!
//! - typing into password / one-time-code / payment-card fields is refused;
//! - submitting a form with such fields, or on a page with a payment iframe, confirms;
//! - clicking something named with a purchase/commit verb (localised list) confirms;
//! - uploads, download starts and category sites are handled by the session.

/// Classify an input as sensitive: `Some("password" | "one-time code" | "payment card")`.
pub fn field_kind(tag: &str, attrs: &[(String, String)]) -> Option<&'static str> {
    if tag != "INPUT" && tag != "TEXTAREA" {
        return None;
    }
    let get = |k: &str| {
        attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.to_ascii_lowercase())
    };
    if get("type").as_deref() == Some("password") {
        return Some("password");
    }
    let ac = get("autocomplete").unwrap_or_default();
    for token in ac.split_whitespace() {
        match token {
            "current-password" | "new-password" => return Some("password"),
            "one-time-code" => return Some("one-time code"),
            t if t.starts_with("cc-") => return Some("payment card"),
            _ => {}
        }
    }
    let ident = format!(
        "{} {}",
        get("name").unwrap_or_default(),
        get("id").unwrap_or_default()
    );
    let ident = ident.replace(['-', '_'], "");
    const CARD: &[&str] = &[
        "cardnumber",
        "ccnumber",
        "cvv",
        "cvc",
        "csc",
        "securitycode",
    ];
    const OTP: &[&str] = &["otp", "onetimecode", "totp", "2fa", "mfacode"];
    if CARD.iter().any(|w| ident.contains(w)) {
        return Some("payment card");
    }
    if OTP
        .iter()
        .any(|w| ident.split_whitespace().any(|t| t == *w || t.contains(w)))
    {
        return Some("one-time code");
    }
    None
}

/// Hosts that serve payment iframes.
const PAYMENT_FRAME_HOSTS: &[&str] = &[
    "js.stripe.com",
    "checkout.stripe.com",
    "pay.google.com",
    "www.paypal.com",
    "assets.braintreegateway.com",
    "checkoutshopper-live.adyen.com",
    "secure.checkout.com",
    "js.squareup.com",
    "applepay.cdn-apple.com",
    "api.razorpay.com",
];

pub fn is_payment_frame(src: &str) -> bool {
    url::Url::parse(src)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| {
            PAYMENT_FRAME_HOSTS
                .iter()
                .any(|p| h == *p || h.ends_with(&format!(".{p}")))
        })
}

/// Purchase / commit verbs (en, de, fr, es, pt, it, nl). Multi-word entries match as
/// consecutive words.
const VERBS: &[&str] = &[
    // en
    "buy",
    "pay",
    "purchase",
    "place order",
    "checkout",
    "check out",
    "subscribe",
    "send",
    "post",
    "publish",
    "delete",
    "transfer",
    "confirm",
    "submit order",
    "donate",
    "book now",
    "order now",
    "unsubscribe",
    "cancel subscription",
    "withdraw",
    // de
    "kaufen",
    "bezahlen",
    "zahlungspflichtig bestellen",
    "jetzt bestellen",
    "senden",
    "veröffentlichen",
    "löschen",
    "überweisen",
    "bestätigen",
    "abonnieren",
    // fr
    "acheter",
    "payer",
    "commander",
    "envoyer",
    "publier",
    "supprimer",
    "virement",
    "confirmer",
    "s'abonner",
    // es
    "comprar",
    "pagar",
    "realizar pedido",
    "enviar",
    "publicar",
    "eliminar",
    "borrar",
    "transferir",
    "confirmar",
    "suscribirse",
    // pt
    "finalizar compra",
    "excluir",
    "assinar",
    // it
    "acquista",
    "paga",
    "ordina",
    "invia",
    "pubblica",
    "elimina",
    "conferma",
    "bonifico",
    // nl
    "kopen",
    "betalen",
    "bestellen",
    "verzenden",
    "verwijderen",
    "bevestigen",
];

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// The commit verb in an element's accessible name, if any.
pub fn commit_verb(name: &str) -> Option<&'static str> {
    let w = words(name);
    VERBS.iter().copied().find(|v| {
        let vw = words(v);
        !vw.is_empty() && w.windows(vw.len()).any(|win| win == vw.as_slice())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn fields() {
        assert_eq!(
            field_kind("INPUT", &a(&[("type", "password")])),
            Some("password")
        );
        assert_eq!(
            field_kind("INPUT", &a(&[("autocomplete", "one-time-code")])),
            Some("one-time code")
        );
        assert_eq!(
            field_kind("INPUT", &a(&[("autocomplete", "billing cc-number")])),
            Some("payment card")
        );
        assert_eq!(
            field_kind("INPUT", &a(&[("name", "card_number")])),
            Some("payment card")
        );
        assert_eq!(
            field_kind("INPUT", &a(&[("id", "otp")])),
            Some("one-time code")
        );
        assert_eq!(
            field_kind("INPUT", &a(&[("type", "email"), ("name", "email")])),
            None
        );
        assert_eq!(field_kind("DIV", &a(&[("type", "password")])), None);
    }

    #[test]
    fn verbs() {
        assert_eq!(commit_verb("Buy now"), Some("buy"));
        assert_eq!(commit_verb("Place order"), Some("place order"));
        assert_eq!(commit_verb("Jetzt kaufen"), Some("kaufen"));
        assert_eq!(commit_verb("Acheter"), Some("acheter"));
        assert_eq!(commit_verb("Sender settings"), None);
        assert_eq!(commit_verb("Paylines"), None);
        assert_eq!(commit_verb("Next"), None);
        assert!(is_payment_frame(
            "https://js.stripe.com/v3/elements-inner.html"
        ));
        assert!(!is_payment_frame("https://example.com/stripe"));
    }
}
