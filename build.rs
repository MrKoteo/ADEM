fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("icon.ico");
        resource.compile().expect("Не удалось встроить иконку");
    }
}
