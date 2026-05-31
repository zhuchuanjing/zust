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

extern "C" fn llm_deep(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let notifier = unsafe { (&*notifier).clone() };
    let id = start_llm_task(value.clone(), || async move {
        let r = llm::complete(openai, value, Some(notifier.clone())).await?;
        llm::notify(&notifier, r.clone())?;
        Ok(r)
    });
    alloc_dynamic(id.into())
}

extern "C" fn llm_image(openai: *const Dynamic, value: *const Dynamic, notifier: *const Dynamic) -> *const Dynamic {
    //启动一个任务 使用 消息点来接收 中间消息
    let openai = unsafe { (&*openai).clone() };
    let value = unsafe { (&*value).clone() };
    let notifier = unsafe { (&*notifier).clone() };
    let id = start_llm_task(value.clone(), || async move {
        let r = llm::image(openai, value, Some(notifier.clone())).await?;
        let _ = llm::notify(&notifier, r.clone());
        Ok(r)
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
