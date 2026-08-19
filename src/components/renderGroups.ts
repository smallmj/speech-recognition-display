/**
 * 双轨展示的渲染分组。
 *
 * 1. 整理中：同一说话人的 active/frozen 片段合并成一个气泡，持续显示汇整原文；
 * 2. 整理完成：同一批次（speakerId + editId）的片段合并成一个整理版气泡；
 * 3. 失败片段保持独立，继续显示原文。
 */

import type { Segment } from "../engineEvents";

export interface RenderSegmentGroup {
  /** 使用批次首个片段 id 作为稳定 key：pending -> cleaned 时不重建 DOM。 */
  key: number;
  speakerId: number;
  primary: Segment;
  segments: Segment[];
  raw: string;
}

export function buildRenderGroups(segments: Segment[]): RenderSegmentGroup[] {
  const groups = new Map<string, RenderSegmentGroup>();
  const ordered: RenderSegmentGroup[] = [];

  for (const seg of segments) {
    const groupKey =
      seg.status === "cleaned" && seg.cleaned != null && seg.editId != null
        ? `cleaned:${seg.speakerId}:${seg.editId}`
        : seg.status === "active" || seg.status === "frozen"
          ? `pending:${seg.speakerId}`
          : `segment:${seg.id}`;

    const existing = groups.get(groupKey);
    if (existing) {
      existing.segments.push(seg);
      existing.raw = [existing.raw, seg.raw].join("\n");
      continue;
    }

    const group: RenderSegmentGroup = {
      key: seg.id,
      speakerId: seg.speakerId,
      primary: seg,
      segments: [seg],
      raw: seg.raw,
    };
    groups.set(groupKey, group);
    ordered.push(group);
  }

  return ordered;
}
