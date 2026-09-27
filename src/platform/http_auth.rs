//! A browser-owned HTML form for a WebKit HTTP Basic or Digest challenge.
//! It is loaded as a simulated response at the challenged origin, so password
//! extensions see an ordinary HTTP(S) document with username/password fields.

use crate::navigation::AuthChallenge;

pub fn origin(challenge: &AuthChallenge) -> Option<String> {
    let scheme = challenge.scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") { return None; }
    let host = challenge.host.trim();
    if host.is_empty() || host.contains(['/', '@', '?', '#']) { return None; }
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = match challenge.port {
        0 | 80 if scheme == "http" => String::new(),
        0 | 443 if scheme == "https" => String::new(),
        1..=65535 => format!(":{}", challenge.port),
        _ => return None,
    };
    let origin = format!("{scheme}://{host}{port}/");
    url::Url::parse(&origin).ok().map(|url| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_has_password_fields_and_escapes_challenge_text() {
        let challenge = AuthChallenge {
            host: "example.com".into(), port: 443, scheme: "https".into(),
            realm: "a <private> & \"quoted\"".into(), failed: true,
        };
        assert_eq!(origin(&challenge).as_deref(), Some("https://example.com/"));
        let html = page(&challenge);
        assert!(html.contains("autocomplete=\"username\""));
        assert!(html.contains("autocomplete=\"current-password\""));
        assert!(html.contains("a &lt;private&gt; &amp; &quot;quoted&quot;"));
        assert!(!html.contains("a <private>"));
        assert!(html.contains("didn’t work"));
    }
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;")
        .replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

pub fn page(challenge: &AuthChallenge) -> String {
    let host = escape(&challenge.host);
    let realm = escape(&challenge.realm);
    let warning = if challenge.failed { "<p class=error>That username or password didn’t work. Try again.</p>" } else { "" };
    let realm = if realm.is_empty() { String::new() } else { format!("<p>Realm: <strong>{realm}</strong></p>") };
    format!(r##"<!doctype html><html><head><meta charset="utf-8"><title>Sign in to {host}</title>
<meta name="color-scheme" content="light dark"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; form-action 'none'">
<style>body{{font:14px -apple-system,system-ui,sans-serif;margin:0;min-height:100vh;display:grid;place-items:center;background:Canvas;color:CanvasText}}main{{width:min(360px,calc(100% - 48px));padding:28px;border:1px solid color-mix(in srgb,CanvasText 20%,transparent);border-radius:14px}}h1{{font-size:22px;margin:0 0 8px}}p{{line-height:1.5;color:color-mix(in srgb,CanvasText 70%,transparent)}}label{{display:block;margin:18px 0 6px}}input{{box-sizing:border-box;width:100%;padding:10px;border:1px solid GrayText;border-radius:7px;background:Field;color:FieldText;font:inherit}}button{{margin-top:22px;padding:10px 18px;border:0;border-radius:7px;background:AccentColor;color:AccentColorText;font:inherit;font-weight:600;cursor:pointer}}.error{{color:#d34a43}}</style>
</head><body><main><h1>Sign in to {host}</h1>{realm}{warning}
<p>Use your password manager or enter the details below. This browser keeps them until you quit.</p>
<form id="auth"><label for="username">Username</label><input id="username" name="username" autocomplete="username" required autofocus>
<label for="password">Password</label><input id="password" name="password" type="password" autocomplete="current-password" required>
<button type="submit">Sign in</button></form></main>
<script>document.getElementById('auth').addEventListener('submit',function(e){{e.preventDefault();window.ipc.postMessage('http-auth:'+JSON.stringify([this.elements.username.value,this.elements.password.value]));}});</script>
</body></html>"##)
}
