use crate::ZustCallback;
use crate::memory::alloc_dynamic;
use anyhow::Result;
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

fn llm_tx(notifier: &Dynamic) -> Option<Dynamic> {
    if notifier.as_custom::<ZustCallback>().is_some() { None } else { Some(notifier.clone()) }
}

fn notify_or_call(notifier: &Dynamic, result: Dynamic) -> Result<()> {
    if let Some(callback) = notifier.as_custom::<ZustCallback>() {
        callback.call1(result)?;
        Ok(())
    } else {
        llm::notify(notifier, result)
    }
}

extern "C" fn llm_deep(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let notifier = unsafe { (&*notifier).clone() };
    let tx = llm_tx(&notifier);
    let id = start_llm_task(value.clone(), || async move {
        let r = llm::complete(openai, value, tx).await?;
        notify_or_call(&notifier, r.clone())?;
        Ok(r)
    });
    alloc_dynamic(id.into())
}

extern "C" fn llm_image(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let notifier = unsafe { (&*notifier).clone() };
    let tx = llm_tx(&notifier);
    let id = start_llm_task(value.clone(), || async move {
        match llm::image(openai, value, tx).await {
            Ok(r) => {
                let _ = notify_or_call(&notifier, r.clone());
                Ok(r)
            }
            Err(err) => {
                let fail = dynamic::map!(
                    "ok" => false,
                    "error" => err.to_string(),
                    "errorDebug" => format!("{err:?}")
                );
                let _ = notify_or_call(&notifier, fail.clone());
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
    fn llm_notifier_can_call_zust_callback() -> anyhow::Result<()> {
        *CALLBACK_RESULT.lock() = None;
        let callback = Dynamic::custom(ZustCallback::new(record_callback_result as *const () as usize, Type::Bool, Vec::new()));

        assert!(llm_tx(&callback).is_none());
        notify_or_call(&callback, dynamic::map!("text"=> "done"))?;

        let result = CALLBACK_RESULT.lock().clone().expect("callback should receive result");
        assert_eq!(result.get_dynamic("text").map(|value| value.as_str().to_string()), Some("done".to_string()));
        Ok(())
    }
}
