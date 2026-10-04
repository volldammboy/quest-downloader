#[cfg(windows)]
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("ProductName", "Quest Downloader");
    res.set("FileDescription", "Quest Downloader - QD");
    if let Err(e) = res.compile() {
        eprintln!("winres: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {}
