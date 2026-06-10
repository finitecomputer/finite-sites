//! Inline HTML for platform-rendered pages (placeholders, login, errors).
//! Site names are DNS labels and emails are validated before they reach
//! these templates, so interpolation cannot inject markup.

const STYLE: &str = "
  body { font-family: -apple-system, system-ui, sans-serif; background: #0b0b0f;
         color: #e8e8ee; display: flex; align-items: center; justify-content: center;
         min-height: 100vh; margin: 0; }
  main { max-width: 26rem; padding: 2rem; text-align: center; }
  h1 { font-size: 1.3rem; margin-bottom: 0.5rem; }
  p { color: #9a9aa8; line-height: 1.5; }
  form { margin-top: 1.5rem; display: flex; gap: 0.5rem; justify-content: center; }
  input[type=email] { padding: 0.6rem 0.8rem; border-radius: 8px; border: 1px solid #33333f;
         background: #16161d; color: #e8e8ee; min-width: 14rem; }
  button { padding: 0.6rem 1rem; border-radius: 8px; border: none;
         background: #5b5bd6; color: white; cursor: pointer; }
  .brand { margin-top: 2.5rem; font-size: 0.8rem; color: #55555f; }
";

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><style>{STYLE}</style></head>\
         <body><main>{body}<p class=\"brand\">finite sites</p></main></body></html>"
    )
}

pub fn unknown_site() -> String {
    page(
        "No such site",
        "<h1>No site lives here</h1>\
         <p>This address is not claimed. It could be yours.</p>",
    )
}

pub fn placeholder(name: &str) -> String {
    page(
        name,
        &format!(
            "<h1>{name} is claimed</h1>\
             <p>Nothing has been published here yet. Check back soon.</p>"
        ),
    )
}

pub fn login(name: &str) -> String {
    page(
        &format!("Sign in to {name}"),
        &format!(
            "<h1>This site is private</h1>\
             <p>If {name} has been shared with you, enter your email and \
             we&rsquo;ll send you a sign-in link.</p>\
             <form method=\"post\" action=\"/_finite/request-link\">\
               <input type=\"email\" name=\"email\" placeholder=\"you@example.com\" required>\
               <button type=\"submit\">Send link</button>\
             </form>"
        ),
    )
}

pub fn link_sent() -> String {
    page(
        "Check your email",
        "<h1>Check your email</h1>\
         <p>If that address has access to this site, a sign-in link is on \
         its way. The link works once and expires in 15 minutes.</p>",
    )
}

pub fn link_invalid() -> String {
    page(
        "Link expired",
        "<h1>That link didn&rsquo;t work</h1>\
         <p>Sign-in links work once and expire after 15 minutes. \
         Request a fresh one from the site&rsquo;s sign-in page.</p>",
    )
}

pub fn app_unavailable() -> String {
    page(
        "App unavailable",
        "<h1>This app isn&rsquo;t responding</h1>\
         <p>It may be starting up or restarting. Refresh in a few seconds.</p>",
    )
}

pub fn not_found() -> String {
    page(
        "Not found",
        "<h1>404</h1><p>This page does not exist on this site.</p>",
    )
}
