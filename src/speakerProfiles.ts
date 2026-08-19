/**
 * 说话人档案 store：`speakerId → { name, avatar }`，localStorage 持久化。
 *
 * T6 手动命名与头像管理 —— 命名/头像是**展示层标注**（engine 不感知），
 * 用户在界面上把自动编号的「说话人 N」改成真实人名、更换头像。
 *
 * 持久化语义（已知限制）：speakerId 由 SCD 每次会话从 1 递增分配；
 * 本 store 按 speakerId 存，会话内稳定、窗口重载/应用重启后 localStorage 恢复。
 * 但**跨会话身份不持久**（ADR-0002「跨天说话人身份持久化」属范围外）——
 * 下次会话的「说话人 1」可能不是上个人，此限制由 T11 会话历史统一解决。
 */

import { useCallback, useState } from "react";

export interface SpeakerProfile {
  /** 自定义显示名；`null` 时显示默认「说话人 N」。 */
  name: string | null;
  /** 头像 emoji。 */
  avatar: string;
}

export type SpeakerProfiles = Record<number, SpeakerProfile>;

const STORAGE_KEY = "scd.speakerProfiles";

/** 可选头像 emoji 集合（手动选择 / 随机换一批共用）。 */
export const AVATARS: string[] = [
  "👨", "👩", "👨‍🦰", "👩‍🦰", "👨‍🦱", "👩‍🦱", "👨‍🦳", "👩‍🦳",
  "👨‍🦲", "👩‍🦲", "🧔", "👧", "🧑", "🙂", "😀", "😊", "🦊", "🐼",
];

/** 按 speakerId 给一个稳定的默认头像（同一 id 恒定，直到用户手动改）。 */
export function defaultAvatar(speakerId: number): string {
  return AVATARS[speakerId % AVATARS.length];
}

/** 取某说话人的档案；未设置时返回默认（名字 null、稳定头像）。 */
export function profileOf(profiles: SpeakerProfiles, speakerId: number): SpeakerProfile {
  return profiles[speakerId] ?? { name: null, avatar: defaultAvatar(speakerId) };
}

function loadProfiles(): SpeakerProfiles {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as SpeakerProfiles;
  } catch {
    console.warn("[speakerProfiles] localStorage 读取失败，使用空档案");
  }
  return {};
}

function saveProfiles(p: SpeakerProfiles): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
  } catch {
    console.warn("[speakerProfiles] localStorage 写入失败");
  }
}

export interface SpeakerProfilesValue {
  profiles: SpeakerProfiles;
  renameSpeaker: (id: number, name: string) => void;
  setSpeakerAvatar: (id: number, avatar: string) => void;
  randomAvatar: (id: number) => void;
}

/**
 * 说话人档案 hook：state + localStorage 持久化。只在渲染树顶层调用一次，
 * 把 profiles 与 mutator 下发给各个说话人徽章，避免多处各自维护状态。
 */
export function useSpeakerProfiles(): SpeakerProfilesValue {
  const [profiles, setProfiles] = useState<SpeakerProfiles>(loadProfiles);

  const renameSpeaker = useCallback((id: number, name: string) => {
    const trimmed = name.trim();
    setProfiles((prev) => {
      const cur = prev[id] ?? { name: null, avatar: defaultAvatar(id) };
      const next = { ...prev, [id]: { ...cur, name: trimmed.length > 0 ? trimmed : null } };
      saveProfiles(next);
      return next;
    });
  }, []);

  const setSpeakerAvatar = useCallback((id: number, avatar: string) => {
    setProfiles((prev) => {
      const cur = prev[id] ?? { name: null, avatar: defaultAvatar(id) };
      const next = { ...prev, [id]: { ...cur, avatar } };
      saveProfiles(next);
      return next;
    });
  }, []);

  const randomAvatar = useCallback((id: number) => {
    setProfiles((prev) => {
      const cur = prev[id] ?? { name: null, avatar: defaultAvatar(id) };
      // 随机换一个，尽量不同于当前
      let next = AVATARS[Math.floor(Math.random() * AVATARS.length)];
      if (next === cur.avatar && AVATARS.length > 1) {
        next = AVATARS[(AVATARS.indexOf(cur.avatar) + 1) % AVATARS.length];
      }
      const out = { ...prev, [id]: { ...cur, avatar: next } };
      saveProfiles(out);
      return out;
    });
  }, []);

  return { profiles, renameSpeaker, setSpeakerAvatar, randomAvatar };
}
