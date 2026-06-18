use crate::ZustCallback;
use crate::memory::alloc_dynamic;
use anyhow::{Result, anyhow};
use dynamic::{Dynamic, Type};
use std::future::Future;

extern "C" fn llm_complete(openai: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let result = root::sync_await!(llm::complete(openai, value, None)).unwrap_or(Dynamic::Null);
    alloc_dynamic(result)
}

extern "C" fn llm_audio(openai: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let text = root::sync_await!(llm::audio_recognize(openai, value)).ok().unwrap_or(Dynamic::Null);
    alloc_dynamic(text)
}

extern "C" fn llm_tts(openai: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let audio = root::sync_await!(llm::tts(openai, value)).ok().unwrap_or(Dynamic::Null);
    alloc_dynamic(audio)
}

fn task_value(id: &str, status: &str, info: Dynamic) -> Dynamic {
    dynamic::map!("id"=> id, "status"=> status, "info"=> info)
}

fn start_llm_task<F, Fut>(info: Dynamic, f: F) -> Dynamic
where
    F: FnOnce() -> Fut + 'static + Send,
    Fut: Future<Output = Result<Dynamic>> + 'static + Send,
{
    let id = uuid::Uuid::new_v4().to_string();
    let path = format!("local/tasks/{}", id);
    let running = task_value(&id, "running", info.deep_clone());
    let done_id = id.clone();
    let done_path = path.clone();
    let done_info = info.deep_clone();
    let runner = async move {
        match f().await {
            Ok(result) => {
                let _ = root::add_value(&done_path, dynamic::map!("id"=> done_id, "status"=> "done", "info"=> done_info, "result"=> result));
                Ok(())
            }
            Err(err) => {
                let _ = root::add_value(&done_path, dynamic::map!("id"=> done_id, "status"=> "error", "info"=> done_info, "error"=> err.to_string()));
                Err(err)
            }
        }
    };

    let object = if tokio::runtime::Handle::try_current().is_ok() {
        root::Object::Task(tokio::task::spawn(runner), running)
    } else {
        root::Object::ThreadTask(
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(runner)
            }),
            running,
        )
    };
    let _ = root::add(&path, object);
    id.into()
}

fn llm_callback(value: &Dynamic, fn_name: &str) -> Result<ZustCallback> {
    value.as_custom::<ZustCallback>().cloned().ok_or_else(|| anyhow!("{fn_name} callback must be closure"))
}

fn callback_arg_error(err: anyhow::Error) -> Dynamic {
    dynamic::map!("ok"=> false, "error"=> err.to_string())
}

extern "C" fn llm_deep(openai: *const Dynamic, value: *const Dynamic, callback: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let callback = unsafe { (&*callback).clone() };
    let callback = match llm_callback(&callback, "llm::deep") {
        Ok(callback) => callback,
        Err(err) => return alloc_dynamic(callback_arg_error(err)),
    };
    let id = start_llm_task(value.clone(), || async move {
        let r = llm::complete(openai, value, None).await?;
        callback.call1(r.clone())?;
        Ok(r)
    });
    alloc_dynamic(id.into())
}

extern "C" fn llm_image(openai: *const Dynamic, value: *const Dynamic, callback: *const Dynamic) -> *const Dynamic {
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let callback = unsafe { (&*callback).clone() };
    let callback = match llm_callback(&callback, "llm::image") {
        Ok(callback) => callback,
        Err(err) => return alloc_dynamic(callback_arg_error(err)),
    };
    let id = start_llm_task(value.clone(), || async move {
        match llm::image(openai, value, None).await {
            Ok(r) => {
                let _ = callback.call1(r.clone());
                Ok(r)
            }
            Err(err) => {
                let fail = dynamic::map!(
                    "ok" => false,
                    "error" => err.to_string(),
                    "errorDebug" => format!("{err:?}")
                );
                let _ = callback.call1(fail.clone());
                Err(err)
            }
        }
    });
    alloc_dynamic(id.into())
}

pub const LLM_NATIVE: [(&str, &[Type], Type, *const u8); 5] = [
    ("complete", &[Type::Any, Type::Any], Type::Any, llm_complete as *const u8),
    ("image", &[Type::Any, Type::Any, Type::Any], Type::Any, llm_image as *const u8),
    ("audio", &[Type::Any, Type::Any], Type::Any, llm_audio as *const u8),
    ("tts", &[Type::Any, Type::Any], Type::Any, llm_tts as *const u8),
    ("deep", &[Type::Any, Type::Any, Type::Any], Type::Any, llm_deep as *const u8),
];

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    static CALLBACK_RESULT: Mutex<Option<Dynamic>> = Mutex::new(None);

    extern "C" fn record_callback_result(result: *const Dynamic) -> bool {
        if result.is_null() {
            return false;
        }
        *CALLBACK_RESULT.lock() = Some(unsafe { &*result }.deep_clone());
        true
    }

    #[test]
    fn llm_async_callback_requires_zust_callback() -> anyhow::Result<()> {
        let error = llm_callback(&Dynamic::from("local/llm/progress"), "llm::deep").expect_err("root notifier path should be rejected");
        assert_eq!(callback_arg_error(error).get_dynamic("ok").and_then(|value| value.as_bool()), Some(false));
        Ok(())
    }

    #[test]
    fn llm_async_callback_can_call_zust_callback() -> anyhow::Result<()> {
        *CALLBACK_RESULT.lock() = None;
        let callback = Dynamic::custom(ZustCallback::new(record_callback_result as *const () as usize, Type::Bool, Vec::new()));

        let callback = llm_callback(&callback, "llm::deep")?;
        callback.call1(dynamic::map!("text"=> "done"))?;

        let result = CALLBACK_RESULT.lock().clone().expect("callback should receive result");
        assert_eq!(result.get_dynamic("text").map(|value| value.as_str().to_string()), Some("done".to_string()));
        Ok(())
    }
}
