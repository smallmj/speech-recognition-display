/**
 * 整理版差异高亮：对比原文 raw 与整理版 cleaned，把 cleaned 中「新增/改动」
 * 的部分标记出来（对齐 ADR-0003 的 CARTGPT 做法：整理版只高亮改动词）。
 *
 * 实现：基于字符级 LCS（最长公共子序列）的简单 diff，不引入 diff 库。
 * 只标记 cleaned 里不在 LCS 中的字符（即 LLM 新增/改写的内容），
 * 用 <mark> 包裹。被删除的原文字符不出现在整理版中，故不标记。
 *
 * 注意：以码点（code point）为单位切分，正确处理中文/emoji 代理对。
 */

export interface DiffRun {
  text: string;
  /** true = 该段为整理版新增/改动的部分（应高亮）。 */
  added: boolean;
}

/**
 * 计算 cleaned 中每个码点是否「新增」（不在与 raw 的 LCS 中）。
 * 返回长度 === Array.from(cleaned).length 的布尔数组。
 */
function computeAddedMask(a: string[], b: string[]): boolean[] {
  const n = a.length;
  const m = b.length;
  // dp[i][j] = a[0..i) 与 b[0..j) 的 LCS 长度
  const dp: number[] = new Array((n + 1) * (m + 1)).fill(0);
  const at = (i: number, j: number) => i * (m + 1) + j;
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      if (a[i - 1] === b[j - 1]) {
        dp[at(i, j)] = dp[at(i - 1, j - 1)] + 1;
      } else {
        dp[at(i, j)] = Math.max(dp[at(i - 1, j)], dp[at(i, j - 1)]);
      }
    }
  }
  const added = new Array<boolean>(m).fill(false);
  // 回溯 LCS：与 raw 匹配的 b 字符不标记；其余（插入/改写）标记为新增。
  let i = n;
  let j = m;
  while (i > 0 && j > 0) {
    if (a[i - 1] === b[j - 1]) {
      i -= 1;
      j -= 1; // LCS 匹配：b[j] 未改动
    } else if (dp[at(i - 1, j)] >= dp[at(i, j - 1)]) {
      i -= 1; // a[i-1] 是删除的原文字符（不在整理版中）
    } else {
      added[j - 1] = true; // b[j-1] 是新增/改写的字符
      j -= 1;
    }
  }
  while (j > 0) {
    added[j - 1] = true; // 剩余 b 前缀均为新增
    j -= 1;
  }
  return added;
}

/**
 * 把 cleaned 与 raw 对比，分成若干段：`added=true` 的段应高亮。
 *
 * - raw === cleaned：整段未改动（不标记）。
 * - cleaned 为空：返回空段，避免渲染异常。
 */
export function diffHighlight(raw: string, cleaned: string): DiffRun[] {
  if (raw === cleaned) {
    return [{ text: cleaned, added: false }];
  }
  const a = Array.from(raw);
  const b = Array.from(cleaned);
  const added = computeAddedMask(a, b);
  const runs: DiffRun[] = [];
  let current: DiffRun | null = null;
  for (let idx = 0; idx < b.length; idx++) {
    const isAdded = added[idx];
    if (current !== null && current.added === isAdded) {
      current.text += b[idx];
    } else {
      current = { text: b[idx], added: isAdded };
      runs.push(current);
    }
  }
  if (runs.length === 0) {
    runs.push({ text: "", added: false });
  }
  return runs;
}
