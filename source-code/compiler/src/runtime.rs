pub fn runtime_c_source() -> &'static str {
    include_str!("../runtime/core.c")
}

pub fn async_rt_c_source() -> &'static str {
    include_str!("../runtime/async_rt.c")
}
