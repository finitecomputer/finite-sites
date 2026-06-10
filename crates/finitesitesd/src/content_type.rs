//! Content types are derived from the served path's extension at response
//! time. Manifests do not carry content types: one fewer field to validate,
//! and the mapping can improve without republishing sites.

pub fn content_type_for_path(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "csv" => "text/csv; charset=utf-8",
        "map" => "application/json",
        // Unknown types download instead of rendering; never sniff.
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_types() {
        assert_eq!(
            content_type_for_path("/index.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type_for_path("/a/b.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_for_path("/app.wasm"), "application/wasm");
        assert_eq!(
            content_type_for_path("/no-extension"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for_path("/weird.xyz"),
            "application/octet-stream"
        );
    }
}
