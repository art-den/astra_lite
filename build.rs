use std::io;

fn main() -> io::Result<()> {
    #[cfg(all(target_os = "windows", not(debug_assertions)))] {
        embed_resource::compile("ui/win_resource.rc");
    }
    Ok(())
}
