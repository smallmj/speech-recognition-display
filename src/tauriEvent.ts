/**
 * 单一 Tauri 事件监听注册（模块级单例）。
 *
 * 问题背景：React 18 StrictMode 在 dev 下会把 effect 挂载→清理→再挂载，
 * 而 Tauri 的 `listen` 是异步返回 `UnlistenFn` 的——清理发生在 Promise resolve
 * 之前，无法反注册，导致同一事件注册了两个监听器（事件回调被触发两次，
 * 气泡重复、ping 重复回执）。
 *
 * 解法：以事件名为键做模块级单例——每个事件名只向 Tauri 注册一次 `listen`，
 * 组件通过 [subscribe] 增删自己的 handler，与 StrictMode 的重复挂载解耦。
 *
 * 监听器随应用生命周期存活（本应用的 WebView 关闭即整体销毁，无需主动反注册）。
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type Handler = (payload: unknown) => void;

interface Entry {
  unlisten: UnlistenFn | null;
  pending: Promise<unknown> | null;
  handlers: Set<Handler>;
}

const registry = new Map<string, Entry>();

function getEntry(event: string): Entry {
  let entry = registry.get(event);
  if (!entry) {
    entry = { unlisten: null, pending: null, handlers: new Set() };
    registry.set(event, entry);
  }
  return entry;
}

/** 确保该事件名已向 Tauri 注册（幂等）。 */
function ensureRegistered(event: string, entry: Entry): void {
  if (entry.pending || entry.unlisten) return;
  entry.pending = listen(event, (evt) => {
    for (const handler of entry.handlers) {
      try {
        handler(evt.payload);
      } catch (err) {
        console.error(`[tauri-event] handler error on ${event}:`, err);
      }
    }
  })
    .then((un) => {
      entry.unlisten = un;
      entry.pending = null;
    })
    .catch((err) => {
      entry.pending = null;
      console.error(`[tauri-event] listen failed on ${event}:`, err);
    });
}

/** 订阅事件；返回取消订阅函数。可被多次调用（StrictMode 安全）。 */
export function subscribe(event: string, handler: Handler): () => void {
  const entry = getEntry(event);
  entry.handlers.add(handler);
  ensureRegistered(event, entry);
  return () => {
    entry.handlers.delete(handler);
  };
}
