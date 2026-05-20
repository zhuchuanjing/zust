mod arrow;
mod directory;
mod mount;
mod node;
pub use arrow::query::{Query, SearchDescriptionResult};
//以后要把内存中的 handler task Sender Receiver 和 数据分开处理
pub use mount::{Mount, Root};

use std::cell::RefCell;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use dynamic::{Dynamic, MsgPack, MsgUnpack, Type};
use rand::RngExt;

use tokio::sync::mpsc;

pub type Msg<T> = (T, Option<mpsc::Sender<T>>);
pub type MsgSender<T> = mpsc::Sender<Msg<T>>;
pub type MsgReceiver<T> = mpsc::Receiver<Msg<T>>;

pub fn tx_rx<T: Send>() -> (MsgSender<T>, MsgReceiver<T>) {
    let (tx, rx) = mpsc::channel(1024);
    (tx, rx)
}

pub fn block_on_async<F, T>(f: F) -> T
where
    F: FnOnce() -> std::pin::Pin<Box<dyn Future<Output = T> + Send>> + 'static + Send,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn(async move {
            let result = f().await;
            let _ = tx.send(result);
        });
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(async { rx.await.unwrap() }))
    } else {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(f())
    }
}

use smol_str::SmolStr;
pub fn start_task<F>(info: Dynamic, f: F) -> Dynamic
where
    F: FnOnce() -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send>> + 'static + Send,
{
    let id = uuid::Uuid::new_v4().to_string();
    let task = SmolStr::new(format!("local/tasks/{}", id));
    let r = if tokio::runtime::Handle::try_current().is_ok() {
        Object::Task(tokio::task::spawn(async move { f().await }), info)
    } else {
        Object::ThreadTask(
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(f())
            }),
            info,
        )
    };
    let _ = add(&task, r);
    log::info!("start task {:?}", task);
    id.into()
}

#[macro_export]
macro_rules! sync_await {
    ($future:expr) => {
        $crate::block_on_async(move || Box::pin($future))
    };
}

pub fn send<T: Send + 'static>(tx: &MsgSender<T>, msg: T) -> Result<()> {
    tx.try_send((msg, None)).map_err(|e| anyhow!("发送失败: {}", e))
}

pub fn call<T: Send + 'static>(tx: &MsgSender<T>, msg: T) -> Result<T> {
    let (reply_tx, mut reply_rx) = mpsc::channel::<T>(1024);
    tx.try_send((msg, Some(reply_tx))).map_err(|e| anyhow!("发送失败: {}", e))?;
    reply_rx.try_recv().map_err(|e| anyhow!("接收回复失败: {}", e))
}

use tokio::task::JoinHandle;

use crate::node::Node;
#[derive(Debug)]
pub enum Object {
    Value(Dynamic),                                           //基本的值
    Native(fn(Dynamic) -> Dynamic),                           //函数处理对象
    Func(i64, Type),                                          //裸指针
    Tx(MsgSender<Dynamic>, Dynamic),                          //包括 Tx 信息
    Task(JoinHandle<Result<()>>, Dynamic),                    //异步任务
    ThreadTask(std::thread::JoinHandle<Result<()>>, Dynamic), //同步任务
}

impl Into<Object> for Dynamic {
    fn into(self) -> Object {
        Object::Value(self)
    }
}

impl Object {
    pub fn value(&self) -> Dynamic {
        match self {
            Self::Value(v) => v.clone(),
            Self::Task(_, info) => info.clone(),
            Self::ThreadTask(_, info) => info.clone(),
            Self::Tx(_, info) => info.clone(),
            _ => Dynamic::Null,
        }
    }
}

impl Default for Object {
    fn default() -> Self {
        Self::Value(Dynamic::Null)
    }
}

impl MsgPack for Object {
    fn encode(&self, buf: &mut Vec<u8>) {
        match self {
            Self::Value(v) => v.encode(buf),
            _ => {}
        }
    }
}

impl MsgUnpack for Object {
    fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        let (v, len) = Dynamic::decode(buf)?;
        Ok((Self::Value(v), len))
    }
}

static ROOT: LazyLock<Root<Object>> = LazyLock::new(|| {
    let root = Root::<Object>::new();
    if let Ok((mount, name)) = root.get_mount("local/fight") {
        mount.add(name, Object::Native(fight));
    }
    root
});

thread_local! {
    static SEND_STACK: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

fn builtin_native(name: &str) -> Option<fn(Dynamic) -> Dynamic> {
    match name {
        "fight" | "native.fight" | "native::fight" => Some(fight),
        _ => None,
    }
}

fn get_value(input: &Dynamic, keys: &[&str]) -> Option<Dynamic> {
    keys.iter().find_map(|key| input.get_dynamic(key))
}

fn get_string(input: &Dynamic, keys: &[&str]) -> Option<String> {
    get_value(input, keys).map(|value| value.as_str().to_string())
}

fn get_i64(input: &Dynamic, keys: &[&str]) -> Option<i64> {
    get_value(input, keys).and_then(|value| value.as_int())
}

fn get_f64(input: &Dynamic, keys: &[&str]) -> Option<f64> {
    get_value(input, keys).and_then(|value| value.as_float())
}

fn get_range(input: &Dynamic, range_keys: &[&str], min_keys: &[&str], max_keys: &[&str], default_min: i64, default_max: i64) -> (i64, i64) {
    if let Some(range) = get_value(input, range_keys) {
        let min = range.get_idx(0).and_then(|v| v.as_int()).unwrap_or(default_min);
        let max = range.get_idx(1).and_then(|v| v.as_int()).unwrap_or(default_max);
        return normalize_range(min, max, default_min, default_max);
    }

    let min = get_i64(input, min_keys).unwrap_or(default_min);
    let max = get_i64(input, max_keys).unwrap_or(default_max);
    normalize_range(min, max, default_min, default_max)
}

fn normalize_range(min: i64, max: i64, default_min: i64, default_max: i64) -> (i64, i64) {
    let min = min.max(0);
    let max = max.max(0);
    if min == 0 && max == 0 {
        (default_min, default_max)
    } else if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

fn role_name(role: &Dynamic) -> String {
    get_string(role, &["name", "名称"]).unwrap_or_else(|| "未命名".to_string())
}

fn role_hp(role: &Dynamic) -> i64 {
    get_i64(role, &["hp", "health", "生命"]).unwrap_or(0).max(0)
}

fn role_max_hp(role: &Dynamic) -> i64 {
    get_i64(role, &["max_hp", "max_health", "最大生命"]).unwrap_or_else(|| role_hp(role)).max(1)
}

fn role_speed(role: &Dynamic) -> i64 {
    get_i64(role, &["speed", "initiative", "速度"]).unwrap_or(0)
}

fn role_attack_range(role: &Dynamic) -> (i64, i64) {
    get_range(role, &["attack", "攻击"], &["attack_min", "min_attack", "攻击下限"], &["attack_max", "max_attack", "攻击上限"], 8, 15)
}

fn role_defense_range(role: &Dynamic) -> (i64, i64) {
    get_range(role, &["defense", "防御"], &["defense_min", "min_defense", "防御下限"], &["defense_max", "max_defense", "防御上限"], 1, 4)
}

fn role_block_chance(role: &Dynamic) -> f64 {
    get_f64(role, &["block_chance", "block", "格挡几率"]).unwrap_or(0.15).clamp(0.0, 0.95)
}

fn role_block_ratio(role: &Dynamic) -> f64 {
    get_f64(role, &["block_ratio", "格挡减伤"]).unwrap_or(0.5).clamp(0.0, 1.0)
}

fn role_crit_chance(role: &Dynamic) -> f64 {
    get_f64(role, &["crit_chance", "crit", "暴击几率"]).unwrap_or(0.1).clamp(0.0, 1.0)
}

fn role_crit_ratio(role: &Dynamic) -> f64 {
    get_f64(role, &["crit_ratio", "暴击倍率"]).unwrap_or(1.5).max(1.0)
}

fn set_i64(role: &Dynamic, key: &str, value: i64) {
    role.set_dynamic(key.into(), value);
}

fn set_bool(role: &Dynamic, key: &str, value: bool) {
    role.set_dynamic(key.into(), value);
}

fn normalize_role(input: &Dynamic, side: &'static str, default_name: &str) -> Dynamic {
    let role = if input.is_map() { input.deep_clone() } else { Dynamic::map(Default::default()) };
    let name = get_string(&role, &["name", "名称"]).unwrap_or_else(|| default_name.to_string());
    let hp = get_i64(&role, &["hp", "health", "生命"]).unwrap_or(100).max(1);
    let (attack_min, attack_max) = role_attack_range(&role);
    let (defense_min, defense_max) = role_defense_range(&role);

    role.set_dynamic("name".into(), name);
    role.set_dynamic("side".into(), side);
    set_i64(&role, "hp", hp);
    set_i64(&role, "max_hp", role_max_hp(&role).max(hp));
    set_i64(&role, "attack_min", attack_min);
    set_i64(&role, "attack_max", attack_max);
    set_i64(&role, "defense_min", defense_min);
    set_i64(&role, "defense_max", defense_max);
    role.set_dynamic("block_chance".into(), role_block_chance(&role));
    role.set_dynamic("block_ratio".into(), role_block_ratio(&role));
    role.set_dynamic("crit_chance".into(), role_crit_chance(&role));
    role.set_dynamic("crit_ratio".into(), role_crit_ratio(&role));
    set_i64(&role, "speed", role_speed(&role));
    set_bool(&role, "alive", hp > 0);
    role
}

fn make_fight_record(entries: [(&str, Dynamic); 10]) -> Dynamic {
    let mut map = std::collections::BTreeMap::<SmolStr, Dynamic>::new();
    for (key, value) in entries {
        map.insert(key.into(), value);
    }
    Dynamic::map(map)
}

fn fight(msg: Dynamic) -> Dynamic {
    let left_input = msg.get_dynamic("left").or_else(|| msg.get_dynamic("a")).unwrap_or(Dynamic::Null);
    let right_input = msg.get_dynamic("right").or_else(|| msg.get_dynamic("b")).unwrap_or(Dynamic::Null);
    if !left_input.is_map() || !right_input.is_map() {
        let mut error = std::collections::BTreeMap::new();
        error.insert("error".into(), "fight expects {left: {...}, right: {...}}".into());
        return Dynamic::map(error);
    }

    let left = normalize_role(&left_input, "left", "左侧");
    let right = normalize_role(&right_input, "right", "右侧");
    let max_rounds = get_i64(&msg, &["max_rounds", "最大回合"]).unwrap_or(50).clamp(1, 500) as usize;

    let mut rng = rand::rng();
    let mut process = Vec::new();
    let mut records = Vec::new();
    let mut left_turn = if role_speed(&left) == role_speed(&right) { rng.random_bool(0.5) } else { role_speed(&left) > role_speed(&right) };

    let mut round_count = 0usize;
    while role_hp(&left) > 0 && role_hp(&right) > 0 && round_count < max_rounds {
        round_count += 1;
        let (attacker, defender) = if left_turn { (&left, &right) } else { (&right, &left) };

        let (attack_min, attack_max) = role_attack_range(attacker);
        let (defense_min, defense_max) = role_defense_range(defender);
        let attack_roll = rng.random_range(attack_min..=attack_max);
        let defense_roll = rng.random_range(defense_min..=defense_max);
        let blocked = rng.random_bool(role_block_chance(defender));
        let crit = rng.random_bool(role_crit_chance(attacker));

        let mut damage = (attack_roll - defense_roll).max(1);
        if crit {
            damage = ((damage as f64) * role_crit_ratio(attacker)).round() as i64;
        }
        if blocked {
            damage = ((damage as f64) * (1.0 - role_block_ratio(defender))).round() as i64;
        }
        damage = damage.max(if blocked { 0 } else { 1 });

        let defender_hp = (role_hp(defender) - damage).max(0);
        let defender_max_hp = role_max_hp(defender);
        set_i64(defender, "hp", defender_hp);
        set_bool(defender, "alive", defender_hp > 0);

        let line = format!(
            "第{}回合 {} 攻击 {}，攻击 {}，防御 {}，{}{}造成 {} 点伤害，{} 剩余 {} / {}",
            round_count,
            role_name(attacker),
            role_name(defender),
            attack_roll,
            defense_roll,
            if crit { "触发暴击，" } else { "" },
            if blocked { "被格挡后，" } else { "" },
            damage,
            role_name(defender),
            defender_hp,
            defender_max_hp,
        );
        process.push(line.clone().into());
        records.push(make_fight_record([
            ("round", (round_count as i64).into()),
            ("attacker", role_name(attacker).into()),
            ("attacker_side", get_string(attacker, &["side"]).unwrap_or_default().into()),
            ("defender", role_name(defender).into()),
            ("defender_side", get_string(defender, &["side"]).unwrap_or_default().into()),
            ("attack_roll", attack_roll.into()),
            ("defense_roll", defense_roll.into()),
            ("blocked", blocked.into()),
            ("critical", crit.into()),
            ("damage", damage.into()),
        ]));
        if let Some(last) = records.last() {
            last.insert("defender_hp", defender_hp);
            last.insert("text", line);
        }

        left_turn = !left_turn;
    }

    let left_hp = role_hp(&left);
    let right_hp = role_hp(&right);
    let left_name = role_name(&left);
    let right_name = role_name(&right);
    let winner = if left_hp == right_hp {
        "draw".to_string()
    } else if left_hp > right_hp {
        left_name.clone()
    } else {
        right_name.clone()
    };
    let loser = if winner == "draw" {
        String::new()
    } else if winner == left_name {
        right_name
    } else {
        left_name
    };

    let mut result = std::collections::BTreeMap::new();
    result.insert("winner".into(), winner.into());
    result.insert("loser".into(), loser.into());
    result.insert("draw".into(), (left_hp == right_hp).into());
    result.insert("round_count".into(), (round_count as i64).into());
    result.insert("process".into(), Dynamic::list(process));
    result.insert("records".into(), Dynamic::list(records));
    result.insert("left".into(), left);
    result.insert("right".into(), right);
    Dynamic::map(result)
}

pub fn mount_memory(name: &str) -> bool {
    ROOT.mount_memory(name)
}

pub fn mount_redis(name: &str, url: &str) -> Result<bool> {
    ROOT.mount_redis(name, url)
}

pub fn mount_fjall(data_dir: &str) -> Result<bool> {
    ROOT.mount_fjall("fjall", data_dir)
}

pub fn get_mount<'a>(name: &'a str) -> Result<(Mount<Object>, &'a str)> {
    ROOT.get_mount(name)
}

pub fn add(name: &str, obj: Object) -> Result<bool> {
    let (m, name) = get_mount(name)?;
    let mut obj = obj;
    let expire = take_object_expire(&mut obj);
    let added = m.add(name, obj);
    if added {
        apply_redis_expire(&m, name, expire);
    }
    Ok(added)
}

pub fn add_native(name: &str, native_name: &str) -> Result<bool> {
    let Some(handler) = builtin_native(native_name) else {
        return Ok(false);
    };
    let (m, name) = get_mount(name)?;
    Ok(m.add(name, Object::Native(handler)))
}

pub fn add_value<T: Into<Dynamic>>(name: &str, val: T) -> Result<bool> {
    let (m, name) = get_mount(name)?;
    let mut value = val.into();
    let expire = take_dynamic_expire(&mut value);
    let added = m.add(name, Object::Value(value));
    if added {
        apply_redis_expire(&m, name, expire);
    }
    Ok(added)
}

pub fn get(name: &str) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    m.get(name, |obj| obj.value())
}

pub fn dir(name: &str) -> Result<Dynamic> {
    let mount_name = name.split_once('/').map(|(n, _)| n).unwrap_or(name);
    let (m, name) = get_mount(name)?;
    m.dir(name).map(|names| if matches!(m, Mount::Redis { .. }) { names.into_iter().map(|n| format!("{}/{}", mount_name, n).into()).collect::<Vec<SmolStr>>().into() } else { names.into() })
}

pub fn contains(name: &str) -> bool {
    if let Ok((m, name)) = get_mount(name) { m.contains(name) } else { false }
}

pub fn remove(name: &str) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    match m.remove(name) {
        Ok(Object::Value(v)) => Ok(v),
        _ => Err(anyhow!("没有删除对象")),
    }
}

pub fn add_list(name: &str) -> Result<()> {
    let (m, name) = get_mount(name)?;
    m.add_list(name);
    Ok(())
}

pub fn push(name: &str, value: Dynamic) -> Result<usize> {
    let (m, name) = get_mount(name)?;
    let mut value = value;
    let expire = take_dynamic_expire(&mut value);
    let len = m.push(name, Object::Value(value))?;
    apply_redis_expire(&m, name, expire);
    Ok(len)
}

pub fn add_map(name: &str) -> Result<()> {
    let (m, name) = get_mount(name)?;
    m.add_map(name);
    Ok(())
}

pub fn insert(name: &str, key: &str, value: Dynamic) -> Result<()> {
    let (m, name) = get_mount(name)?;
    let mut value = value;
    let expire = take_dynamic_expire(&mut value);
    let _ = m.insert(name, key, Object::Value(value));
    apply_redis_expire(&m, name, expire);
    Ok(())
}

fn take_object_expire(obj: &mut Object) -> Option<i64> {
    if let Object::Value(value) = obj { take_dynamic_expire(value) } else { None }
}

fn take_dynamic_expire(value: &mut Dynamic) -> Option<i64> {
    value.remove_dynamic("@expire").and_then(|expire| expire.as_int()).filter(|expire| *expire > 0)
}

fn apply_redis_expire(mount: &Mount<Object>, name: &str, expire: Option<i64>) {
    let Some(expire) = expire else {
        return;
    };
    if let Mount::Redis { client, rl: _ } = mount
        && let Ok(mut conn) = client.get_connection()
    {
        let _ = conn.expire::<&str, ()>(name, expire);
    }
}

pub fn get_key(name: &str, key: &str) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    m.get_key(name, key, |obj| obj.value())
}

use redis::Commands;
pub fn get_list(name: &str) -> Result<Vec<Dynamic>> {
    let (m, name) = get_mount(name)?;
    match m {
        Mount::Memory(m) => m
            .read_sync(name, |_, v| match v {
                Node::List(l) => l.iter().map(|(_, item)| item.value()).collect(),
                _ => Vec::new(),
            })
            .ok_or(anyhow!("未发现 {}", name)),
        Mount::Redis { client, rl: _ } => {
            let mut conn = client.get_connection()?;
            let items: Vec<Vec<u8>> = conn.lrange(name, 0, -1)?;
            let items: Vec<Dynamic> = items.into_iter().map(|buf| Dynamic::decode(buf.as_slice()).map(|(v, _)| v).unwrap_or(Dynamic::Null)).collect();
            Ok(items)
        }
        Mount::Fjall { .. } => {
            let len = m.len(name)?;
            let mut items = Vec::new();
            for idx in 0..len {
                if let Ok(value) = m.get_idx(name, idx, |obj| obj.value()) {
                    items.push(value);
                }
            }
            Ok(items)
        }
    }
}

#[derive(Debug)]
enum MyFn {
    Null,
    Sender(MsgSender<Dynamic>),
    Native(fn(Dynamic) -> Dynamic),
    Script(i64, Type),
}

impl MyFn {
    fn call(&self, msg: Dynamic) -> Result<Dynamic> {
        match self {
            MyFn::Sender(tx) => call(tx, msg),
            MyFn::Native(f) => Ok(f(msg)),
            MyFn::Script(ptr, ty) => dynamic::call_fn(*ptr, ty.clone(), Box::new(msg)).map(|r| r.as_ref().clone()),
            MyFn::Null => Ok(Dynamic::Null),
        }
    }
}

impl From<&Object> for MyFn {
    fn from(obj: &Object) -> Self {
        match obj {
            Object::Tx(tx, _) => MyFn::Sender(tx.clone()),
            Object::Task(t, _) => {
                t.abort();
                MyFn::Null
            }
            Object::Func(ptr, ty) => MyFn::Script(*ptr, ty.clone()),
            Object::Native(f) => MyFn::Native(*f),
            _ => MyFn::Null,
        }
    }
}

pub fn send_msg(name: &str, msg: Dynamic) -> Result<Dynamic> {
    let entered = SEND_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.len() >= 64 {
            return Err(anyhow!("root::send call depth exceeded at {}", name));
        }
        if stack.iter().any(|item| item == name) {
            return Err(anyhow!("root::send recursive path detected: {}", name));
        }
        stack.push(name.to_string());
        Ok(())
    });
    entered?;
    let result = (|| {
        let (m, name) = get_mount(name)?;
        let f: MyFn = m.get(name, |obj| obj.into())?;
        f.call(msg)
    })();
    SEND_STACK.with(|stack| {
        let _ = stack.borrow_mut().pop();
    });
    result
}

pub fn send_idx_msg(name: &str, idx: usize, msg: Dynamic) -> Result<Dynamic> {
    let stack_name = format!("{}[{}]", name, idx);
    let entered = SEND_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.len() >= 64 {
            return Err(anyhow!("root::send_idx call depth exceeded at {}", stack_name));
        }
        if stack.iter().any(|item| item == &stack_name) {
            return Err(anyhow!("root::send_idx recursive path detected: {}", stack_name));
        }
        stack.push(stack_name);
        Ok(())
    });
    entered?;
    let result = (|| {
        let (m, name) = get_mount(name)?;
        let f: MyFn = m.get_idx(name, idx, |obj| obj.into())?;
        f.call(msg)
    })();
    SEND_STACK.with(|stack| {
        let _ = stack.borrow_mut().pop();
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fight_request() -> Dynamic {
        let left = Dynamic::map(Default::default());
        left.insert("name", "战士");
        left.insert("hp", 30);
        left.insert("attack_min", 8);
        left.insert("attack_max", 12);
        left.insert("defense_min", 1);
        left.insert("defense_max", 3);
        left.insert("block_chance", 0.2);

        let right = Dynamic::map(Default::default());
        right.insert("name", "盗贼");
        right.insert("hp", 24);
        right.insert("attack_min", 6);
        right.insert("attack_max", 10);
        right.insert("defense_min", 0);
        right.insert("defense_max", 2);
        right.insert("block_chance", 0.1);

        let msg = Dynamic::map(Default::default());
        msg.insert("left", left);
        msg.insert("right", right);
        msg
    }

    #[test]
    fn add_native_registers_builtin_fight_handler() {
        assert!(add_native("local/test/fight_handler", "native.fight").unwrap());
        let result = send_msg("local/test/fight_handler", fight_request()).unwrap();
        assert!(result.is_map());
        assert!(result.get_dynamic("winner").is_some());
        assert!(result.get_dynamic("process").is_some_and(|v| v.is_list()));
    }

    #[test]
    fn local_fight_handler_is_available_by_default() {
        assert!(contains("local/fight"));
        let result = send_msg("local/fight", fight_request()).unwrap();
        let rounds = result.get_dynamic("round_count").and_then(|v| v.as_int()).unwrap_or_default();
        assert!(rounds > 0);
    }

    #[test]
    fn take_dynamic_expire_strips_positive_expire_metadata() {
        let mut value = dynamic::map!("@expire"=> 30, "name"=> "zust");

        assert_eq!(take_dynamic_expire(&mut value), Some(30));
        assert!(!value.contains("@expire"));
        assert_eq!(value.get_dynamic("name").unwrap().as_str(), "zust");
    }

    #[test]
    fn take_dynamic_expire_ignores_non_positive_expire_metadata() {
        let mut value = dynamic::map!("@expire"=> 0, "name"=> "zust");

        assert_eq!(take_dynamic_expire(&mut value), None);
        assert!(!value.contains("@expire"));
    }
}
