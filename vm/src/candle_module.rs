use crate::memory::alloc_dynamic;
use dynamic::{Dynamic, Type};

extern "C" fn candle_embed(options: *const Dynamic, input: *const Dynamic) -> *const Dynamic {
    let options = unsafe { (&*options).clone() };
    let input = unsafe { (&*input).clone() };
    let result = match llm::candle::embed(options, input) {
        Ok(result) => result,
        Err(err) => dynamic::map!("ok"=> false, "error"=> err.to_string()),
    };
    alloc_dynamic(result)
}

extern "C" fn candle_load_embedder(options: *const Dynamic) -> *const Dynamic {
    let options = unsafe { (&*options).clone() };
    let result = match llm::candle::load_embedder(options) {
        Ok(result) => result,
        Err(err) => dynamic::map!("ok"=> false, "error"=> err.to_string()),
    };
    alloc_dynamic(result)
}

pub const CANDLE_NATIVE: [(&str, &[Type], Type, *const u8); 2] = [("embed", &[Type::Any, Type::Any], Type::Any, candle_embed as *const u8), ("load_embedder", &[Type::Any], Type::Any, candle_load_embedder as *const u8)];
