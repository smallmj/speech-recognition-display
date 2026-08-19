/**
 * 说话人徽章：头像 + 名字（可改名）+ 头像选择器（换一批 / 手动选）。
 * 纯渲染组件：档案与 mutator 由上层（DualTrackView）经 `useSpeakerProfiles` 下发。
 */

import { useEffect, useRef, useState } from "react";
import { AVATARS, type SpeakerProfile } from "../speakerProfiles";

interface SpeakerBadgeProps {
  speakerId: number;
  color: string;
  profile: SpeakerProfile;
  onRename: (id: number, name: string) => void;
  onSetAvatar: (id: number, avatar: string) => void;
  onRandomAvatar: (id: number) => void;
}

export default function SpeakerBadge({
  speakerId,
  color,
  profile,
  onRename,
  onSetAvatar,
  onRandomAvatar,
}: SpeakerBadgeProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const pickerRef = useRef<HTMLDivElement>(null);

  const displayName = profile.name ?? `说话人 ${speakerId}`;

  // 进入编辑态自动聚焦
  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  // 头像选择器打开时，点击外部关闭
  useEffect(() => {
    if (!pickerOpen) return;
    function onDocDown(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    }
    document.addEventListener("mousedown", onDocDown);
    return () => document.removeEventListener("mousedown", onDocDown);
  }, [pickerOpen]);

  function commitRename() {
    onRename(speakerId, draft);
    setEditing(false);
  }

  return (
    <span className="dual-speaker" style={{ color }}>
      <button
        type="button"
        className="speaker-avatar"
        onClick={() => setPickerOpen((v) => !v)}
        title="换头像"
        aria-label={`说话人 ${speakerId} 换头像`}
      >
        {profile.avatar}
      </button>

      {pickerOpen && (
        <div className="speaker-avatar-picker" ref={pickerRef}>
          <div className="speaker-avatar-grid">
            {AVATARS.map((e) => (
              <button
                key={e}
                type="button"
                className={`speaker-avatar-opt ${e === profile.avatar ? "is-on" : ""}`}
                onClick={() => {
                  onSetAvatar(speakerId, e);
                  setPickerOpen(false);
                }}
              >
                {e}
              </button>
            ))}
          </div>
          <button
            type="button"
            className="speaker-avatar-shuffle"
            onClick={() => {
              onRandomAvatar(speakerId);
              setPickerOpen(false);
            }}
          >
            🎲 换一批
          </button>
        </div>
      )}

      {editing ? (
        <input
          ref={inputRef}
          className="speaker-name-input"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commitRename}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitRename();
            if (e.key === "Escape") setEditing(false);
          }}
          placeholder={`说话人 ${speakerId}`}
        />
      ) : (
        <button
          type="button"
          className="speaker-name"
          onClick={() => {
            setDraft(profile.name ?? "");
            setEditing(true);
          }}
          title="点击改名"
        >
          {displayName}
        </button>
      )}
    </span>
  );
}
