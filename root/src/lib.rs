mod arrow;
mod directory;
mod mount;
mod node;
pub use arrow::query::{Query, SearchDescriptionResult};
//以后要把内存中的 handler task Sender Receiver 和 数据分开处理
pub use mount::{Mount, Root};

use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

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
            // spawn 任务若 panic,tx 在 send 前被 drop,rx.await 会收到 Err,
            // 由下面的 match 转成带上下文的 panic,而非裸 unwrap 丢失信息。
            let result = f().await;
            let _ = tx.send(result);
        });
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(async {
            match rx.await {
                Ok(value) => value,
                // rx 收到 Err 说明 tx 被 drop(spawn 任务 panic 或提前退出):带上下文 panic。
                Err(_) => panic!("block_on_async: spawn 任务在发送结果前异常退出"),
            }
        }))
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
    let (reply_tx, reply_rx) = mpsc::channel::<T>(1024);
    tx.try_send((msg, Some(reply_tx))).map_err(|e| anyhow!("发送失败: {}", e))?;
    // 原先用 try_recv() 立即返回,但处理方在另一 task 里异步消费,根本没机会在
    // try_send 和 try_recv 之间执行,导致几乎必然返回"接收回复失败"。
    // 改为阻塞等待 reply:把 reply_rx move 进 async 块,复用 block_on_async 的
    // block_in_place 机制等待回复。
    sync_await!(async move {
        let mut rx = reply_rx;
        rx.recv().await
    })
    .ok_or_else(|| anyhow!("接收回复失败: channel 关闭"))
}

use tokio::task::JoinHandle;

use crate::node::Node;

/// ROOT 挂载的 native 处理函数。
///
/// 用 `Arc<dyn Fn>` 而非 `fn` 指针,这样既能挂无状态的顶层 `fn`,
/// 也能挂带捕获环境的闭包(`Arc::new(move |msg| { ... 用到外部状态 ... })`)。
/// newtype + 手动 Debug 让 `Object` 仍可派生 Debug(`Arc<dyn Fn>` 本身不实现 Debug)。
#[derive(Clone)]
pub struct NativeHandler(Arc<dyn Fn(Dynamic) -> Dynamic + Send + Sync>);

impl NativeHandler {
    /// 用顶层 `fn` 构造(零开销,等价于原先的 `fn` 指针)。
    pub fn from_fn(f: fn(Dynamic) -> Dynamic) -> Self {
        Self(Arc::new(f))
    }

    /// 用任意 `Fn` 闭包构造(可带捕获,需 `Send + Sync`)。
    pub fn from_closure<F>(f: F) -> Self
    where
        F: Fn(Dynamic) -> Dynamic + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// 调用处理函数。
    pub fn call(&self, msg: Dynamic) -> Dynamic {
        (self.0)(msg)
    }
}

impl std::fmt::Debug for NativeHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHandler").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Object {
    Value(Dynamic),                                           //基本的值
    Native(NativeHandler),                                    //函数处理对象(支持 fn 与闭包)
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
            Self::Value(v) => v.deep_clone(),
            Self::Task(_, info) => info.deep_clone(),
            Self::ThreadTask(_, info) => info.deep_clone(),
            Self::Tx(_, info) => info.deep_clone(),
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
        mount.add(name, Object::Native(NativeHandler::from_fn(fight)));
    }
    root
});

thread_local! {
    static SEND_STACK: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

fn builtin_native(name: &str) -> Option<NativeHandler> {
    match name {
        "fight" | "native.fight" | "native::fight" => Some(NativeHandler::from_fn(fight)),
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

pub fn mount_fjall(name: &str, data_dir: &str) -> Result<bool> {
    ROOT.mount_fjall(name, data_dir)
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

/// `add_native` 第二个参数的 trait 重载。
///
/// 既支持按内建名挂载(`add_native("local/h", "fight")`,查 `builtin_native`),
/// 也支持直接挂任意 `Fn` 闭包(`add_native("local/h", |msg| { ... })`,可带捕获)。
/// 顶层 `fn` 也会走闭包分支(无捕获闭包)。
pub trait IntoNativeHandler {
    fn into_handler(self) -> Option<NativeHandler>;
}

/// `&str`:按内建名查找(如 "fight")。未知名返回 None,`add_native` 返回 false。
impl IntoNativeHandler for &str {
    fn into_handler(self) -> Option<NativeHandler> {
        builtin_native(self)
    }
}

/// 任意 `Fn(Dynamic) -> Dynamic + Send + Sync` 闭包(含无捕获的顶层 `fn`)。
/// 允许 `root::add_native("local/h", |msg| { ... })` 直接挂闭包。
impl<F> IntoNativeHandler for F
where
    F: Fn(Dynamic) -> Dynamic + Send + Sync + 'static,
{
    fn into_handler(self) -> Option<NativeHandler> {
        Some(NativeHandler::from_closure(self))
    }
}

/// 挂载一个 native 处理函数到 ROOT 树。
///
/// 第二个参数既可以是内建名(`&str`,如 "fight"),也可以是任意
/// `Fn(Dynamic) -> Dynamic + Send + Sync` 闭包(可带捕获环境)。
///
/// 注意:示例为说明用途,需在已初始化 ROOT 树的上下文中调用。
///
/// ```no_run
/// # use dynamic::Dynamic;
/// # use root;
/// // 按名挂内建
/// let _ = root::add_native("local/fight", "fight");
/// // 挂顶层 fn
/// fn h(msg: Dynamic) -> Dynamic { Dynamic::Null }
/// let _ = root::add_native("local/h", h);
/// // 挂带捕获的闭包
/// let state = 42i64;
/// let closure = move |msg: Dynamic| -> Dynamic { Dynamic::from(state) };
/// let _ = root::add_native("local/h2", closure);
/// ```
pub fn add_native<H: IntoNativeHandler>(name: &str, handler: H) -> Result<bool> {
    let Some(handler) = handler.into_handler() else {
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
    let (m, name) = get_mount(name)?;
    m.dir(name).map(Into::into)
}

pub fn keys(name: &str) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    m.keys(name).map(Into::into)
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

/// 按 idx 从列表移除元素(符合 sparse slot 设计:不 pack,索引稳定)。
/// 用于 WS 连接等需要稳定 idx 的场景,断开时回收列表槽位避免无限增长。
pub fn remove_idx(name: &str, idx: usize) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    match m.remove_idx(name, idx)? {
        Object::Value(v) => Ok(v),
        _ => Ok(Dynamic::Null),
    }
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
    // expire 是附加属性,失败不应让 add/insert 报错(主数据已写入),
    // 但必须记录日志,否则过期未设置会静默导致 key 永驻。
    if let Mount::Redis { client, rl: _ } = mount
        && let Ok(mut conn) = client.get_connection()
    {
        if let Err(e) = conn.expire::<&str, ()>(name, expire) {
            log::warn!("redis expire {} {}s 失败: {e:#}", name, expire);
        }
    }
}

pub fn get_key(name: &str, key: &str) -> Result<Dynamic> {
    let (m, name) = get_mount(name)?;
    m.get_key(name, key, |obj| obj.value())
}

/// 原子并发更新一个节点的值。返回更新后的值。
/// 节点必须已存在,否则返回 Err。
/// 原子并发更新一个 Object 节点的值。返回更新后的值。
///
/// **警告**:闭包 `f` 在 Memory 后端持有 scc bucket 级写锁(自旋锁,不可重入)期间执行。
/// 闭包内**不要再调用任何 ROOT 操作**(如 `root::get`/`root::update`/`root::send_msg`),
/// 否则若目标 name 与当前 name 落入同一 hash bucket,会自旋死锁。
/// 闭包应只对传入的 `Dynamic` 做本地计算与修改。
pub fn update<F>(name: &str, mut f: F) -> Result<Dynamic>
where
    F: FnMut(Dynamic) -> Dynamic + Send + 'static,
{
    let (m, name) = get_mount(name)?;
    m.get_mut(name, move |obj| match obj {
        Object::Value(v) => {
            let current = std::mem::take(v);
            let next = f(current);
            let result = next.deep_clone();
            *v = next;
            result
        }
        _ => Dynamic::Null,
    })
}

/// 原子并发更新一个 map 节点内某个 key 的值。返回更新后的值。
///
/// **警告**:同 [`update`],闭包在 Memory 后端持有写锁期间执行,
/// 闭包内不要回调 ROOT 操作,否则可能自旋死锁。
pub fn update_key<F>(name: &str, key: &str, mut f: F) -> Result<Dynamic>
where
    F: FnMut(Dynamic) -> Dynamic + Send + 'static,
{
    let (m, name) = get_mount(name)?;
    m.get_key_mut(name, key, move |obj| match obj {
        Object::Value(v) => {
            let current = std::mem::take(v);
            let next = f(current);
            let result = next.deep_clone();
            *v = next;
            result
        }
        _ => Dynamic::Null,
    })
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
    Native(NativeHandler),
    Script(i64, Type),
}

impl MyFn {
    fn call(&self, msg: Dynamic) -> Result<Dynamic> {
        match self {
            MyFn::Sender(tx) => call(tx, msg),
            MyFn::Native(f) => Ok(f.call(msg)),
            MyFn::Script(ptr, ty) => dynamic::call_fn(*ptr, ty.clone(), Box::new(msg)).map(|r| r.as_ref().clone()),
            MyFn::Null => Ok(Dynamic::Null),
        }
    }
}

impl From<&Object> for MyFn {
    fn from(obj: &Object) -> Self {
        match obj {
            Object::Tx(tx, _) => MyFn::Sender(tx.clone()),
            Object::Task(_, _) => {
                // Task 节点不可作为函数调用;读它不应有 abort 副作用(否则 send_msg
                // 读取 Task 句柄会杀掉正在运行的后台任务)。
                MyFn::Null
            }
            Object::Func(ptr, ty) => MyFn::Script(*ptr, ty.clone()),
            Object::Native(f) => MyFn::Native(f.clone()),
            _ => MyFn::Null,
        }
    }
}

/// SEND_STACK 的 RAII guard:无论 send_msg/send_idx_msg 的调用是正常返回还是 panic,
/// Drop 都会弹出栈帧,避免 thread_local 栈残留导致后续误判"递归路径检测"。
struct SendStackGuard;
impl Drop for SendStackGuard {
    fn drop(&mut self) {
        SEND_STACK.with(|stack| {
            let _ = stack.borrow_mut().pop();
        });
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
    // guard 在此作用域结束(含 panic unwind)时 Drop,保证 SEND_STACK 不残留。
    let _guard = SendStackGuard;
    let (m, name) = get_mount(name)?;
    let f: MyFn = m.get(name, |obj| obj.into())?;
    f.call(msg)
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
    let _guard = SendStackGuard;
    let (m, name) = get_mount(name)?;
    let f: MyFn = m.get_idx(name, idx, |obj| obj.into())?;
    f.call(msg)
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
    fn add_native_accepts_closure_with_captured_state() {
        // 挂带捕获环境的闭包:原先 Object::Native 是 fn 指针,不支持闭包;
        // 改成 NativeHandler(Arc<dyn Fn>) 后应能挂任意 Fn 闭包。
        let multiplier: i64 = 10;
        let closure = move |msg: Dynamic| -> Dynamic {
            let n = msg.as_int().unwrap_or(0);
            Dynamic::from(n * multiplier)
        };
        assert!(add_native("local/test/closure_handler", closure).unwrap());
        let result = send_msg("local/test/closure_handler", Dynamic::from(5i64)).unwrap();
        assert_eq!(result.as_int(), Some(50));
    }

    #[test]
    fn add_native_accepts_bare_fn() {
        // 顶层 fn 也应能直接挂(走 Fn 闭包分支)
        fn echo(msg: Dynamic) -> Dynamic {
            msg
        }
        assert!(add_native("local/test/echo", echo).unwrap());
        let result = send_msg("local/test/echo", Dynamic::from(42i64)).unwrap();
        assert_eq!(result.as_int(), Some(42));
    }

    #[test]
    fn add_native_unknown_builtin_name_returns_false() {
        // 未知名按 &str 分支返回 None,add_native 返回 false 而非报错
        assert!(!add_native("local/test/unknown", "no_such_builtin").unwrap());
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

    #[test]
    fn update_increments_value_atomically() {
        let path = format!("local/test/update_inc/{}", uuid::Uuid::new_v4());
        add_value(&path, 0_i64).unwrap();

        let next = update(&path, |v| v + Dynamic::I64(1)).unwrap();
        assert_eq!(next.as_int(), Some(1));
        assert_eq!(get(&path).unwrap().as_int(), Some(1));

        let next = update(&path, |v| v + Dynamic::I64(41)).unwrap();
        assert_eq!(next.as_int(), Some(42));
        assert_eq!(get(&path).unwrap().as_int(), Some(42));
    }

    #[test]
    fn update_concurrent_threads_no_lost_update() {
        let path = format!("local/test/update_concurrent/{}", uuid::Uuid::new_v4());
        add_value(&path, 0_i64).unwrap();

        const THREADS: i64 = 8;
        const ITERS: i64 = 1000;

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..ITERS {
                    update(&p, |v| v + Dynamic::I64(1)).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(get(&path).unwrap().as_int(), Some(THREADS * ITERS));
    }

    #[test]
    fn update_key_increments_map_value() {
        let path = format!("local/test/update_key/{}", uuid::Uuid::new_v4());
        add_map(&path).unwrap();
        insert(&path, "n", 0_i64.into()).unwrap();

        const THREADS: i64 = 8;
        const ITERS: i64 = 500;

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..ITERS {
                    update_key(&p, "n", |v| v + Dynamic::I64(1)).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(get_key(&path, "n").unwrap().as_int(), Some(THREADS * ITERS));
    }

    #[test]
    fn keys_returns_memory_map_keys() {
        let path = format!("local/test/keys/{}", uuid::Uuid::new_v4());
        add_map(&path).unwrap();
        insert(&path, "0", "zero".into()).unwrap();
        insert(&path, "1", "one".into()).unwrap();

        let key_list = keys(&path).unwrap();
        let mut keys = (0..key_list.len()).map(|idx| key_list.get_idx(idx).unwrap().as_str().to_string()).collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, vec!["0".to_string(), "1".to_string()]);
    }

    #[test]
    fn dir_returns_immediate_child_names() {
        let path = format!("local/test/public_dir/{}", uuid::Uuid::new_v4());
        add_value(&format!("{path}/a"), 1_i64).unwrap();
        add_value(&format!("{path}/sub/item"), 2_i64).unwrap();

        let entries = dir(&path).unwrap();
        let mut entries = (0..entries.len()).map(|idx| entries.get_idx(idx).unwrap().as_str().to_string()).collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["a".to_string(), "sub".to_string()]);
    }

    #[test]
    fn mount_fjall_uses_explicit_mount_name() {
        let data_dir = std::env::temp_dir().join(format!("zust-root-fjall-named-{}", uuid::Uuid::new_v4()));
        let data_dir_str = data_dir.to_str().unwrap();
        let mount_name = format!("fjall_{}", uuid::Uuid::new_v4().simple());

        assert!(mount_fjall(&mount_name, data_dir_str).unwrap());
        add_value(&format!("{mount_name}/item"), 9_i64).unwrap();
        assert_eq!(get(&format!("{mount_name}/item")).unwrap().as_int(), Some(9));
        assert!(!contains("fjall/item"));

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn update_returns_err_when_missing() {
        let path = format!("local/test/update_missing/{}", uuid::Uuid::new_v4());
        let result = update(&path, |v| v + Dynamic::I64(1));
        assert!(result.is_err());
    }

    #[test]
    fn root_get_returns_independent_dynamic_values() {
        let path = format!("local/test/root_get_clone/{}", uuid::Uuid::new_v4());
        let nested = dynamic::map!("score"=> 1);
        let value = dynamic::map!("nested"=> nested.clone(), "items"=> Dynamic::list(vec![nested]));

        add_value(&path, value).unwrap();

        let first = get(&path).unwrap();
        first.get_dynamic("nested").unwrap().insert("score", 2);
        first.get_dynamic("items").unwrap().get_idx(0).unwrap().insert("score", 3);

        let second = get(&path).unwrap();
        assert_eq!(second.get_dynamic("nested").unwrap().get_dynamic("score").and_then(|v| v.as_int()), Some(1));
        assert_eq!(second.get_dynamic("items").unwrap().get_idx(0).unwrap().get_dynamic("score").and_then(|v| v.as_int()), Some(1));
    }
}
