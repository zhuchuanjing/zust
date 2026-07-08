# 游戏平台 Manifest 契约（v0.1 草案）

本文定义「AI 创作的多人叙事游戏集市」平台中，**一个游戏（摊位）在服务端 ROOT 里的数据结构**和**必须实现的 zust handler 签名**。

这份契约是整个服务端的骨架：

- LLM 按它生成 zust 游戏包；
- VM 按它加载并注册 handler；
- 框架（`on_action` 调度）按它强制「zust 持锁，LLM 只产文本」；
- 客户端按它读 manifest + 可见 state 渲染；
- 集市、社交、配额、embedding 都挂在它的字段上。

所有路径基于现有 `root` crate（`root::add` / `get` / `insert` / `update` / `update_key` / `dir` / `keys` / `send_msg` / `add_fn`），不引入新 ROOT 原语。

---

## 1. 设计原则（不可妥协）

### 1.1 zust 持锁，LLM 产文本

`on_action` 是**唯一**能写 `world` 状态的入口。`narrate` 是**唯一**能调 LLM 的函数，且它的返回值**只能是文本或结构化选择题**，绝不能直接写 ROOT。

LLM 的输出回到 `on_action`，由 zust 二次判定后才决定是否写状态。

### 1.2 强类型校验，不用 JS 风格 fallback

所有外部输入（玩家动作、manifest 字段）都显式校验类型。缺失字段直接返回业务错误，不给全局默认值（遵循 AGENTS.md 1.2.7）。

### 1.3 必需数据缺失要明确失败

manifest 的必需字段缺失，游戏加载阶段就失败，不进入集市。玩家动作的必需字段缺失，handler 返回 `{ok: false, error}`。

### 1.4 每个多写业务动作要么在一个事务里完成，要么不写

`on_action` 内的所有 state 变更（世界状态、任务进度、实例计数）必须原子完成，使用 `root::update_key` 的闭包形式持有写锁。

---

## 2. ROOT 命名空间总览

平台在 ROOT 里维护以下顶层命名空间。每个 mount 是独立的（参考现有 `mount_memory` / `mount_fjall` 用法）：

```
presence/      集市化身实时位置与状态（memory mount，低延迟）
social/        好友、屏蔽、好友请求（fjall 持久）
activities/    围观/房间/会话的实时活动树（memory）
games/         所有游戏实例的权威状态树（fjall 持久）
catalog/       游戏目录元数据 + embedding 向量（fjall）
quota/         LLM 调用配额（fjall）
accounts/      跨游戏统一身份（fjall）
local/         HTTP / WS 分发入口（memory，由 start.zs 注册）
```

`presence` 和 `activities` 是低延迟、可丢失的运行期状态，挂 memory。`games`、`social`、`catalog`、`quota`、`accounts` 是权威持久状态，挂 fjall。这与现有 `mount_fjall` commit 157af4b 的方向一致。

---

## 3. 游戏在 ROOT 里的结构

每个游戏占用 `games/{game_id}/` 子树。`game_id` 是平台分配的稳定字符串 ID（如 `"dengying"` 或 ulid）。

```
games/{game_id}/
  manifest            游戏元数据 map（必需，见 §4）
  world/              这个游戏的权威世界状态
    scenes/           场景定义
    npcs/             NPC 状态
    flags/            任务、关系、世界布尔
    items/            物品定义
  instances/          进行中的实例（每个房间独立进度）
    {instance_id}/
      members         list[uid]
      local_state     map（实例局部状态副本）
      rules_version   u32
      started_at      i64
      status          "waiting" | "playing" | "closed"
  handlers/           handler 注册点（由框架注册，见 §5）
    on_action
    narrate
    on_join
    on_leave
  stats/              供集市和创作者看的统计
    plays             u32
    spectators_total  u32
    rating_sum        f64
    rating_count      u32
```

**实例隔离**：同一个游戏可同时有多个房间。每个 `instance_id` 在 `instances/{instance_id}/local_state` 维护自己的进度副本，互不干扰。玩家 `on_action` 必须带 `instance_id`，handler 据此路由到正确的局部状态。

---

## 4. manifest 字段定义

manifest 是一个 map，由 LLM 在创作时生成，平台在加载时校验。**必需字段缺失 → 拒绝发布**。

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `game_id` | string | 是 | 平台分配的稳定 ID |
| `title` | string | 是 | 显示名（集市门面招牌） |
| `description` | string | 是 | 一段描述，用于 embedding 和集市展示 |
| `creator_uid` | string | 是 | 创作者账号 |
| `created_at` | i64 | 是 | 创建时间戳 |
| `rules_version` | u32 | 是 | 当前脚本版本号，用于实例冻结 |
| `time_model` | string | 是 | `"turn"` \| `"realtime"` \| `"state_machine"`，决定调度方式 |
| `capacity` | u32 | 是 | 单实例最大玩家数（≥1） |
| `genre` | string | 是 | `"narrative"` \| `"social_deduction"` \| `"puzzle"` \| `"sim"` \| `"other"` |
| `entry_handlers` | map | 是 | 见 §5，handler 模块路径 |
| `narrate_model` | map | 否 | LLM 配置（模型、温度等），缺省用平台默认 |
| `stall_template` | string | 否 | 集市门面模板（`"house"`\|`"tower"`\|`"tent"`\|`"waterside"`\|`"signboard"`） |
| `stall_palette` | map | 否 | 门面配色与招牌图 URL |
| `initial_world` | map | 是 | `world/` 的初始值，由 LLM 生成 |
| `skill_metric` | map | 否 | 「小白-高手」维度定义，见 §7 |

**类型校验示例（zust）**：

```zs
fn validate_manifest(m) {
  let required = ["game_id", "title", "description", "creator_uid",
                  "created_at", "rules_version", "time_model",
                  "capacity", "genre", "entry_handlers", "initial_world"];
  for i in 0..required.len() {
    let key = required[i];
    if !m.contains(key) {
      return { ok: false, error: "manifest missing: " + key };
    }
  }
  let cap = m.capacity;
  if !cap.is_int() || cap < 1 {
    return { ok: false, error: "capacity must be int >= 1" };
  }
  let tm = m.time_model;
  if tm != "turn" && tm != "realtime" && tm != "state_machine" {
    return { ok: false, error: "unknown time_model: " + tm };
  }
  return { ok: true };
}
```

注意：不写 `let cap = m.capacity || 1` 这种 JS fallback。显式校验，明确失败。

---

## 5. handler 契约

每个游戏必须实现以下 handler。LLM 生成游戏时按这个签名产代码，平台编译时校验符号存在，运行时由框架 dispatcher 调用。

handler 通过 `root::add_fn` 注册到 `games/{game_id}/handlers/{name}`，dispatcher 用 `root::send_msg` 调用。

### 5.1 `on_action(req) -> resp`（必需）

玩家动作的唯一入口。**唯一能写 world/local_state 的函数**。

**输入**（dispatcher 注入，玩家不可伪造）：

```zs
{
  @ws: true,                  // 传输元数据
  uid: "xiaozhe",             // 已鉴权玩家 uid（dispatcher 从 session 注入）
  game_id: "dengying",
  instance_id: "room_abc",
  action: {
    kind: "choose",           // "choose" | "free_text" | "look" | "interact" | "leave"
    option_id: "talk_to_boss" // 或 { text: "我想问..." }（kind=free_text 时）
  }
}
```

**处理流程（强制顺序）**：

1. 读 instance local_state。
2. `check_rules(state, action)` 判定合法性。
3. 非法 → 返回 `{ok: false, error}`，**不写状态**。
4. 合法 → `apply_action(state, action)` 计算新状态。
5. 如果想让玩家看到一段场景文字，调 `runtime::narrate(...)` 生成文本（就是一次 LLM 调用，可选）。**文本只是文本，不写状态。**
6. 在一次 `root::update_key` 闭包里写回新状态。
7. 返回 `{ok: true, text?, state: visible_state(new_state), options}`。

**关键约束**：状态变更永远在 zust 手里（第 4、6 步）。LLM 生成的文字（第 5 步）只是给玩家看的描述，绝不反向影响状态。这就是「zust 持锁，LLM 产文本」的全部含义——一句话，不是一个模块。

**返回结构**：

```zs
{
  ok: true,
  state: { ... },             // 当前玩家可见的世界状态
  text: "...",                // 可选，LLM 生成的场景文字（玩家屏幕显示的旁白）
  options: [                  // 可选，下一步可选项
    { id: "talk_to_boss", label: "和老板搭话" }
  ]
}
// 或
{ ok: false, error: "你还没有那把钥匙" }
```

### 5.2 `on_join(req) -> resp`（必需）

玩家加入实例（凑热闹/参与）。

```zs
// 输入
{ uid, game_id, instance_id, role: "player"|"spectator" }
// 输出
{ ok: true, members: [...], state: visible_state(...) }
// 或
{ ok: false, error: "instance full" }
```

`on_join` 负责把 uid 写入 `instances/{id}/members`（或旁观者列表），返回当前可见状态。**满员、已开始、被屏蔽**等情况在这里判。

### 5.3 `on_leave(req) -> resp`（必需）

玩家离开实例（断线、主动退出、踢出）。

```zs
// 输入
{ uid, game_id, instance_id, reason: "manual"|"disconnect" }
// 输出
{ ok: true }
```

`on_leave` 负责从 members 移除。若实例空了且 `status=waiting` 超过 5 分钟，由框架的清理任务关闭实例。

---

## 6. dispatcher：框架强制的调度

游戏不直接暴露 `on_action` 给 WS。平台有一个**框架级 dispatcher**，所有玩家动作先进 dispatcher，dispatcher 再调游戏的 `on_action`。这一层负责：

1. **鉴权**：从 session 注入可信 `uid`，玩家伪造的 `uid` 被覆盖。
2. **配额**：调 `narrate` 前查 `quota/players/{uid}/llm_narrate_credits`，不够则降级为「不调 LLM，返回规则默认文本」。
3. **审计**：记 `action_log`（uid、game_id、action kind、是否调了 LLM、耗时）。
4. **广播**：`on_action` 返回后，dispatcher 把 state diff 推给实例成员和旁观者。
5. **异常兜底**：游戏 handler panic 或超时，dispatcher 返回 `{ok:false, error:"internal"}`，不让游戏脚本崩整个平台。

dispatcher 本身是一个 zust 模块（如 `runtime::dispatch_action`），在 `start.zs` 里注册到 WS 入口：

```zs
// start.zs 片段
root::add_fn("local/ws_handlers/message", "runtime::on_ws_message");
root::add_fn("local/http/post/api/action", "runtime::on_http_action");
```

`runtime::on_ws_message` 根据 msg.type 分发到 `dispatch_action` / `chat` / `plaza_move` 等。

---

## 7. 「小白-高手」技能维度

manifest 的可选字段 `skill_metric`，由 LLM 在创作时为这个游戏定义：

```zs
skill_metric: {
  axis: "exploration_depth",   // 这个游戏的熟练度叫什么
  compute: "skill::dengying",  // 计算函数的模块路径
  levels: [                    // 分段，供地图和徽章用
    { max: 0.2, label: "初来乍到" },
    { max: 0.6, label: "熟客" },
    { max: 1.0, label: "老江湖" }
  ]
}
```

`compute` 指向一个 zust 函数 `fn(state, history) -> f64`，返回 0.0-1.0。每游戏的熟练度语义不同（叙事游戏看分支探索深度，解谜看通关时间），所以必须由游戏自己声明，平台不统一。

---

## 8. 集市门面（stall）

manifest 的 `stall_template` + `stall_palette` 决定游戏在 3D 集市里的门面外观。这是**轻量信号**，不是 3D 创作：

```zs
stall_template: "house",
stall_palette: {
  primary: "#c0392b",
  accent:  "#f39c12",
  sign_url: "https://cdn.../dengying_sign.png"
}
```

平台客户端有固定的几种门面 3D 模板（house/tower/tent/waterside/signboard），创作者只选模板 + 配色 + 上传一张招牌图。**创作者不建模，LLM 不生成 3D 资产**。这保证 AI 创作不被 3D 工程绑架。

---

## 9. 创作管线（authoring）

`POST /api/author/create` → `runtime::author_create`：

```zs
fn author_create(req) {
  // 1. 配额检查
  let quota = root::get("quota/creators/" + req.uid + "/llm_create_credits");
  if !quota.is_int() || quota < 1 {
    return { ok: false, error: "no create credits" };
  }

  // 2. 调 LLM 生成游戏包（manifest + initial_world + rules + narrate 模板）
  let pkg = llm::complete(llm_options, build_author_prompt(req.description));
  // pkg 应包含 manifest, world_init, rules_code, narrate_code

  // 3. 校验 manifest
  let v = validate_manifest(pkg.manifest);
  if !v.ok { return v; }

  // 4. 分配 game_id，注册到 catalog
  let game_id = assign_id();
  root::insert("catalog", game_id, pkg.manifest);

  // 5. 编译游戏脚本进 VM（沙箱）
  let compiled = vm::compile_sandboxed(pkg.rules_code, pkg.narrate_code);
  if !compiled.ok {
    return { ok: false, error: "compile failed: " + compiled.error };
  }

  // 6. smoke test：模拟几个动作，看是否 panic / 死循环 / 返回非法结构
  let smoke = run_smoke_test(game_id);
  if !smoke.ok {
    root::remove("catalog/" + game_id);
    return { ok: false, error: "smoke test failed: " + smoke.error };
  }

  // 7. 算 embedding，存 catalog/{game_id}/embedding
  let emb = llm::embed_local(pkg.manifest.description);  // KaLM 本地
  root::insert("catalog/" + game_id + "/embedding", emb);

  // 8. 扣配额
  root::update_key("quota/creators", req.uid, |q| { q.llm_create_credits -= 1; q });

  return { ok: true, game_id };
}
```

**当前阶段用框架层约束，不做 VM 沙箱**。zust 脚本层本身能力受限：没有文件读写、没有网络 IO，能跨边界的只有 ROOT。需要堵的口子只有三个，都是框架层（zust 脚本 + dispatcher）能做的，不需要动 vm crate：

1. **`runtime::narrate` 是唯一 LLM 入口**。`llm` crate 不注册给游戏脚本的符号表，游戏只能调平台预注册的 `runtime::narrate`。这样游戏脚本物理上调不到 `llm::complete`。
2. **dispatcher 强制 instance 路径前缀**。游戏 `on_action` 写 ROOT 前，dispatcher 检查路径必须匹配 `games/{game_id}/instances/{instance_id}/`。
3. **创作管线编译前扫 AST**。拒绝含 `std::spawn` / `std::sleep` / `import` 的脚本（LLM 产的游戏不需要这些）。

真正的 VM 沙箱（CPU 时间 / 内存 / 调用深度限制）推到后期，它防的是「LLM 产了死循环脚本占满 CPU」这类服务质量问题，不是安全问题，可以后置。

---

## 10. 配额与反滥用

```
quota/
  creators/{uid}/
    llm_create_credits    i32   每天可生成游戏数
    reset_at              i64
  players/{uid}/
    llm_narrate_credits   i32   每天可触发的 narration 数
    reset_at              i64
  games/{game_id}/
    daily_llm_budget      i32   单游戏每天 LLM 总调用上限
    total_llm_calls       i64
```

每次 LLM 调用前后必须经过 dispatcher 的 quota 检查。**配额不够时降级而不是报错**——narration 走规则默认文本，创作返回明确错误。

---

## 11. 集市与社交（仅列字段，契约边界）

集市（presence/activities）和社交（social/accounts）是平台层，**不属于单个游戏的 manifest**，但游戏通过 dispatcher 与它们交互：

- `on_join` 成功 → dispatcher 写 `presence/plaza/{uid}.activity_at = game_id`，更新 `activities/...`。
- 玩家在实例里 → dispatcher 在 `presence/plaza/{uid}.status = "in_game"`。
- 玩家位置变化 → dispatcher 更新 `presence/plaza/{uid}.pos`。
- 好友 presence 推送 → 由 `social` 模块的独立任务订阅 presence 变化推给好友。

这些不放在 manifest 里，因为它们是平台行为，不是游戏自描述。

---

## 12. 一个最小游戏包示例（LLM 产出目标）

下面是一个 LLM 应该产出的最小 zust 游戏包，所有游戏包都长这个样子：

```zs
// file: dengying.zs —— 由 LLM 生成

// === manifest（注册时由平台存入 catalog/{game_id}）===
pub fn manifest() {
  return {
    game_id: "dengying",
    title: "灯影客栈",
    description: "一个深夜的乡村小酒馆，老板是个退休的盗贼，来的客人都有自己的秘密。最多 4 人。",
    genre: "narrative",
    time_model: "turn",
    capacity: 4,
    stall_template: "house",
    stall_palette: { primary: "#c0392b", accent: "#f39c12" },
    skill_metric: {
      axis: "exploration_depth",
      compute: "dengying::skill",
      levels: [
        { max: 0.3, label: "新客" },
        { max: 0.7, label: "熟客" },
        { max: 1.0, label: "客栈通" }
      ]
    }
  };
}

// === initial_world（实例创建时拷贝到 instances/{id}/local_state）===
pub fn initial_world() {
  return {
    scene: "tavern",
    flags: { ghost_appeared: false, boss_trust: 0, cage_uncovered: false },
    npcs: { boss: { mood: "calm", knows_secret: false } },
    discovered_branches: []
  };
}

// === on_action（dispatcher 调用，唯一写状态入口）===
pub fn on_action(req) {
  let inst_path = "games/" + req.game_id + "/instances/" + req.instance_id + "/local_state";
  let state = root::get(inst_path);

  // 1. 合法性
  let check = check_rules(state, req.action);
  if !check.ok {
    return { ok: false, error: check.error };
  }

  // 2. 应用（纯计算，不写 ROOT）
  let new_state = apply_action(state, req.action);

  // 3. 可选：让 LLM 写一段场景文字给玩家看（就是一次 LLM 调用，不写状态）
  let text = "";
  if action.kind == "choose" {
    text = runtime::narrate({
      game_id: req.game_id,
      state: snapshot_for_llm(new_state),
      action: req.action
    });
  }

  // 4. 一次写回（事务）。状态变更只在 zust 这一步，text 不参与。
  root::update(inst_path, |_old| { new_state });

  return {
    ok: true,
    state: visible_state(new_state),
    text: text,
    options: available_options(new_state)
  };
}

// === 游戏自实现的规则函数（不暴露给 dispatcher）===
fn check_rules(state, action) {
  if action.kind == "choose" {
    let opts = available_options(state);
    // 校验 option_id 在 opts 里
    for i in 0..opts.len() {
      if opts[i].id == action.option_id {
        return { ok: true };
      }
    }
    return { ok: false, error: "invalid option" };
  }
  return { ok: true };
}

fn apply_action(state, action) {
  // 纯函数：state + action -> new_state
  // 这里不放任何 root:: 写操作
  let s = state;
  if action.kind == "choose" && action.option_id == "talk_to_boss" {
    s.flags.boss_trust = s.flags.boss_trust + 1;
    s.recent_events = push_event(s.recent_events, "talked_to_boss");
  }
  if action.kind == "choose" && action.option_id == "uncover_cage" {
    s.flags.cage_uncovered = true;
    s.flags.ghost_appeared = true;
  }
  return s;
}

fn available_options(state) {
  let opts = [];
  opts = push(opts, { id: "talk_to_boss", label: "和老板搭话" });
  if !state.flags.cage_uncovered {
    opts = push(opts, { id: "look_cage", label: "查看蒙布的笼子" });
    opts = push(opts, { id: "uncover_cage", label: "揭开笼子的布" });
  }
  opts = push(opts, { id: "leave", label: "离开客栈" });
  return opts;
}

fn visible_state(state) {
  // 返回给玩家可见的字段（隐藏秘密，如 boss.knows_secret）
  return {
    scene: state.scene,
    flags: { ghost_appeared: state.flags.ghost_appeared },
    npcs: { boss: { mood: state.npcs.boss.mood } }
  };
}

pub fn skill(state, _history) {
  // 0.0 - 1.0，供集市「小白-高手」坐标
  return (state.discovered_branches.len() as f64) / 10.0;
}

// on_join / on_leave 由框架提供默认实现，游戏可选覆盖
pub fn on_join(req) {
  return runtime::default_on_join(req);
}
pub fn on_leave(req) {
  return runtime::default_on_leave(req);
}
```

---

## 13. 当前缺口清单（按优先级）

| 缺口 | 层 | 说明 |
|---|---|---|
| **本契约文档定稿** | 设计 | 本文 v0.1，需评审 |
| **`runtime::` 框架模块** | zust 脚本 | dispatcher、default_on_join、narrate（唯一 LLM 入口）、quota 检查、smoke test、路径前缀约束、AST 过滤 |
| **manifest 校验 + 加载** | zust 脚本 | `validate_manifest` + 把游戏包编译进 VM 并注册 handlers |
| **VM 资源配额（后置）** | vm crate | CPU/内存/调用深度限制，防 LLM 死循环脚本。**当前不做**，因 zust 脚本层已无 fs/network 能力 |
| **presence + AOI** | zust + Rust | 集市心跳，可独立于本契约先做 |
| **社交图 + 好友推送** | zust 脚本 | ROOT 树 + WS 推送 |
| **embedding 入 catalog** | zust 脚本 | 调 KaLM 本地 embed，存向量 |

---

## 14. 待决问题

1. **`runtime::narrate` 如何提供给游戏脚本**：游戏 `on_action` 调 `runtime::narrate(...)`。由于 `llm` crate 不注册给游戏脚本符号表，游戏物理上只能走 `runtime::narrate`。待定的是：`runtime` 作为平台预注册的 native 模块（推荐，配额/审计在 Rust 侧），还是作为平台自有的 zust 模块（配额/审计在 zust 侧）。前者性能好且游戏脚本看不到实现，后者纯 zust 可审。
2. **实例 local_state 的并发**：同一实例多个玩家同时 on_action，`root::update` 的闭包锁是否足够隔离。需要验证 root crate 的 `update` 是否串行化同路径写。
3. **smoke test 的覆盖度**：自动跑几个动作能不能覆盖大多数崩溃路径，还是要 LLM 同时产「测试用例」。
