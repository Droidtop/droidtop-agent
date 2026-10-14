fn main() {
    // An extension module leaves Python's symbols to the interpreter that
    // loads it; macOS's linker has to be told so.
    pyo3_build_config::add_extension_module_link_args();
}
