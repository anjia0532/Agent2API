import * as React from 'react'
import { Card, MetricCard, MetricGrid, MetricLabel, MetricValue } from '@ui'
import { formatCny, formatInt, formatPercent, formatTokens, type StatsSummary } from '../report-charts'
import { t } from '../../i18n'
import { measured, sumUsage } from './usage-data'
import { CompactValue } from './compact-value'

export function ReportOverview({ summary, loading }: { summary: StatsSummary | null; loading: React.ReactNode }) {
  const overview = summary?.overview
  const fallback = sumUsage((summary?.clients || []).map(row => ({ ...row })))
  const sourceComplete = summary?.clients && summary.clients.reduce((sum, row) => sum + (row.requests || 0), 0) === overview?.requests
  const cached = measured(overview?.cacheReadTokens) ?? (sourceComplete ? measured(fallback.cacheReadTokens) : null)
  const input = measured(overview?.promptTokens) ?? (sourceComplete ? measured(fallback.promptTokens) : null)
  const rate = input && cached !== null ? formatPercent(cached / input) : '—'
  const requests = overview?.requests || 0
  const success = overview?.successful || 0
  const cells = [
    { key: 'tokens', label: t('总 Token'), value: <CompactValue value={overview?.tokens} />, sub: t('输入 + 输出') },
    { key: 'requests', label: t('总请求数'), value: <CompactValue value={requests} />, sub: t('{n} 次请求', { n: formatInt(requests) }) },
    { key: 'success', label: t('成功请求数'), value: <CompactValue value={success} />, sub: t('成功率 {rate} · 失败 {n} 次', { rate: requests ? formatPercent(success / requests) : '—', n: formatInt(Math.max(0, requests - success)) }) },
    { key: 'cache', label: t('缓存读取'), value: cached === null ? '—' : <CompactValue value={cached} />, sub: cached === null ? t('历史明细不完整') : t('占输入 Token 的 {rate}', { rate }) },
  ]
  // ── 估算成本（第 5 格）──────────────────────────────────────
  //
  // 三条显示规则，都是为了让这个数字**不被误当成账单**：
  //  1. 旧网关不返回 `cost`（或价表读不出来 → `available:false`）时**不加这一格**：
  //     显示一个恒为 ¥0 的卡片，用户会以为真的没花钱；
  //  2. 一条都没定价时值显示 `—` 而不是 `¥0` —— 那是「没算出来」，不是「零成本」；
  //  3. 有未定价的条目时把条数写进副标题：只给合计会让人以为覆盖了全部流量。
  const cost = summary?.cost
  const priced = Number(cost?.priced) || 0
  const missing = Number(cost?.missingCount) || 0
  if (cost && cost.available !== false) {
    const allUnpriced = priced <= 0 && missing > 0
    cells.push({
      key: 'cost',
      label: t('估算成本'),
      value: allUnpriced ? <span>—</span> : <span>{formatCny(cost.cny)}</span>,
      sub: allUnpriced
        ? t('{n} 条未定价', { n: formatInt(missing) })
        : missing > 0
          ? t('按挂牌价折算 · {n} 条未定价', { n: formatInt(missing) })
          : t('按挂牌价折算，不是账单'),
    })
  }
  return <Card render={<section />} className='report-overview-block' aria-label={t('统计概览')}>
    <MetricGrid className='report-overview-grid' id='report-overview'>
      {summary ? cells.map(cell => <MetricCard className='report-stat' key={cell.key}>
        <MetricLabel>{cell.label}</MetricLabel><MetricValue>{cell.value}</MetricValue><div className='report-stat-caption'>{cell.sub}</div>
      </MetricCard>) : loading}
    </MetricGrid>
    {summary ? <div className='report-overview-secondary'>
      <span>{t('活跃天数')} <strong>{formatInt(overview?.activeDays)}</strong><small>/ {formatInt(summary.dailyTrend?.length || 0)}</small></span>
      <span>{t('当前连续天数')} <strong>{formatInt(overview?.streak)}</strong></span>
      <span className='report-top-model'>{t('Top 模型')} <strong title={overview?.topModel?.model}>{overview?.topModel?.model || '—'}</strong>
        {overview?.topModel ? <small>{formatTokens(overview.topModel.tokens)} · {formatPercent(overview.topModel.percentage)}</small> : null}</span>
    </div> : null}
  </Card>
}
