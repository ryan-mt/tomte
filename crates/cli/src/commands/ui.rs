use anyhow::Result;

pub async fn run(port: u16, open_browser: bool) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}");
    println!("🌐  Starting Web UI at {url}");
    if open_browser {
        let _ = webbrowser::open(&url);
    }
    crate::server::serve(port).await
}
