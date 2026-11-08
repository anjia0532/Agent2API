//! 崩溃现场：把 panic 的信息**写进日志**（issue #463）。
//!
//! ── 问题 ────────────────────────────────────────────────────
//! `[profile.release]` 里有 `panic = "abort"`（见 `src-tauri/Cargo.toml`），
//! 配合 `strip = true`，panic 在 Windows 上表现为托盘图标突然消失 +
//! 事件日志一条 `BEX64 / FAST_FAIL_FATAL_APP_EXIT`，**正文、位置、线程全没有**。
//! 用户侧只能靠 `procdump` 挂转储才能拿到调用栈 —— 而正常人不会为了报个 bug
//! 去挂调试器。
//!
//! ── 关键事实：abort 之前 panic hook **仍然会跑** ──────────────
//! `panic = "abort"` 只是跳过**展开**（unwind），`std::panic::set_hook` 装的
//! 钩子照常在 abort 前执行。所以不换掉 abort 策略（`opt-level="s"` +
//! `lto` + `codegen-units=1` 的体积/启动收益是刻意要的），也能补上现场。
//!
//! ── 这套钩子**抓不到**什么（必须说清楚，否则会误导排查）──────
//! **内存分配失败不走 panic**：`handle_alloc_error` 直接 abort，不经过 panic
//! 机制。那条路径要拿现场得包一层 `GlobalAlloc`（分配返回 null 时先记一笔）。
//! 本次没有改分配器 —— 它在每次分配的热路径上，而这个仓库的分配器又按平台
//! 分了两套（Linux 用 jemalloc，桌面端用默认），改动面与收益不成比例。
//! 所以：**本模块负责 panic，OOM 仍无现场**，这一点写在下面 `install` 的注释里。
//!
//! ── 为什么要写成可测的纯函数 ────────────────────────────────
//! 真正的钩子只能在 panic 时跑，单测里没法「触发一次真 panic 再断言文件」。
//! 所以把「把 panic 信息拼成一行」抽成 [`describe`]，钩子只负责调用它再落盘。

use std::any::Any;
use std::panic::PanicHookInfo;

/// 一条崩溃描述最多保留多少字符。
///
/// panic 的 payload 是任意 `String`，理论上能是几 MB（有人把整个响应体塞进
/// `panic!("{}", body)`）。日志行过长的后果是写入时再分配、在 OOM 场景下
/// **二次失败**，于是什么都留不下 —— 这正是要避免的。
const MAX_MESSAGE_CHARS: usize = 2_000;

/// 把 panic 的 payload 与位置拼成一行可读文本。
///
/// 纯函数，便于单测覆盖各种 payload 形态（`&str` / `String` / 其它类型）。
pub fn describe(payload: &(dyn Any + Send), location: Option<&std::panic::Location<'_>>) -> String {
    let message = if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        // 非字符串 payload：`panic_any` 传了自定义类型。稳定库里拿不到它的
        // **具体类型名**（`type_name_of_val` 对 trait object 只会给出
        // `&dyn Any + Send`），所以退一步打印 `TypeId` —— 它不是人读的名字，
        // 但同一类型每次崩溃都一样，足以把「是不是同一个 panic」对上。
        // 说成「非字符串」而不是留空，是因为留空等于没有现场。
        let id = payload.type_id();
        format!("<非字符串 panic payload：TypeId {id:?}>")
    };
    let message = truncate(&message, MAX_MESSAGE_CHARS);
    match location {
        Some(at) => format!("{message}（位置：{}:{}:{}）", at.file(), at.line(), at.column()),
        None => message,
    }
}

/// 按**字符**（不是字节）截断，避免把一个多字节字符切成两半。
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("…（已截断）");
    out
}

/// 钩子的实际动作：拼描述 → 写日志 → 写 stderr。
fn report(info: &PanicHookInfo<'_>) {
    let text = describe(info.payload(), info.location());
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("<未命名线程>");
    // 用 ERROR 级：这是进程即将退出的最后一条记录，不该被默认级别过滤掉
    crate::server::logging::log(
        "[Crash]",
        &format!("进程即将终止（panic = abort）｜线程 {name}｜{text}"),
    );
    // 同时写 stderr：Docker / systemd / 桌面端带控制台启动时，日志文件可能
    // 还没初始化或已经关掉，这一行是唯一还能出去的通路
    eprintln!("[Crash] 线程 {name} panic：{text}（进程即将终止）");
}

/// 装崩溃钩子。**必须在任何可能 panic 的代码之前调用**（各入口 `main` 的第一行）。
///
/// ── 幂等 ──────────────────────────────────────────────────
/// 桌面端与服务端共用同一份库，两边都会调；`set_hook` 本身是覆盖语义，
/// 重复调用只会换成等价的新钩子，没有副作用，所以这里不加「只装一次」的锁。
///
/// ── 明确不做的事 ──────────────────────────────────────────
/// 不捕获、不吞掉 panic：钩子返回之后进程照旧 abort。它只负责**留现场**，
/// 不改变崩溃行为 —— 半死不活的进程比干脆退出更难查。
pub fn install() {
    std::panic::set_hook(Box::new(report));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `panic!("字面量")` 的 payload 是 `&str`（`&'static str`），必须能读出来。
    #[test]
    fn a_str_payload_is_read() {
        let payload: &(dyn Any + Send) = &"上游返回了空响应";
        assert_eq!(describe(payload, None), "上游返回了空响应");
    }

    /// `panic!("{}", x)` 的 payload 是 `String`，同样要能读。
    #[test]
    fn a_string_payload_is_read() {
        let owned = format!("模型 {} 不可用", "glm-5.3-flash");
        let payload: &(dyn Any + Send) = &owned;
        assert_eq!(describe(payload, None), "模型 glm-5.3-flash 不可用");
    }

    /// 非字符串 payload 不能变成空白：至少要说清「是什么」，且同一类型要可复现。
    ///
    /// 稳定库拿不到 trait object 的具体类型名，所以这里只断言 `TypeId` 出现、
    /// 且**同一类型两次描述一致**（能用来判断「是不是同一个 panic」）。
    #[test]
    fn a_non_string_payload_still_says_something() {
        let payload: &(dyn Any + Send) = &42u32;
        let first = describe(payload, None);
        let second = describe(payload, None);
        assert!(first.contains("非字符串"), "实际：{first}");
        assert!(first.contains("TypeId"), "应当带上 TypeId，实际：{first}");
        assert_eq!(first, second, "同一类型的描述必须稳定，否则对不上号");
    }

    /// 位置要拼进去（值是 `file:line:col`），这是排查最要紧的一格。
    #[test]
    fn the_location_is_appended() {
        let location = std::panic::Location::caller();
        let payload: &(dyn Any + Send) = &"boom";
        let text = describe(payload, Some(&location));
        assert!(text.starts_with("boom（位置："), "实际：{text}");
        assert!(text.contains(".rs:"), "应当含文件名与行号，实际：{text}");
    }

    /// 超长 payload 要截断，且**不能把一个多字节字符切开**。
    #[test]
    fn an_overlong_payload_is_truncated_on_char_boundaries() {
        let long = "汉".repeat(MAX_MESSAGE_CHARS + 50);
        let payload: &(dyn Any + Send) = &long;
        let text = describe(payload, None);
        assert!(text.ends_with("（已截断）"), "实际结尾：…{}", &text[text.len().saturating_sub(20)..]);
        // 截断后的主体仍是合法 UTF-8 且长度正好是上限（说明是按字符数的）
        let body = text.trim_end_matches("…（已截断）");
        assert_eq!(body.chars().count(), MAX_MESSAGE_CHARS);
    }

    /// 恰好等于上限时不截断（边界不该凭空多一句「已截断」）。
    #[test]
    fn exactly_at_the_limit_is_not_truncated() {
        let exact = "a".repeat(MAX_MESSAGE_CHARS);
        let payload: &(dyn Any + Send) = &exact;
        assert!(!describe(payload, None).contains("已截断"));
    }
}
