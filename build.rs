fn main() {
    println!("cargo:rerun-if-changed=assets/soundx.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/soundx.ico");
        resource.set("FileDescription", "soundx audio processor");
        resource.set("ProductName", "soundx");
        resource.set("OriginalFilename", "soundx.exe");
        resource.compile().expect("failed to embed soundx icon");
    }
}
