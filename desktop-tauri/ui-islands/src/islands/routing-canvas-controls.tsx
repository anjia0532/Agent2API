import * as React from 'react'
import { Badge, Button, Checkbox, Popover, PopoverContent, PopoverTrigger, Switch, cn } from '@ui'
import { t } from '../i18n'
import { PROVIDER_ICONS } from './add-provider-pick'
import { displayNameOf, providerOf } from './accounts-domain'
import type { AccountRecord, AccountSorting } from './accounts-shared'
import { RoutingCreditExpiry } from './routing-account-order'
import { RouteIcon, RouteStateBadge } from './routing-ui'

export type RoutingAction = { label: string; icon?: string; disabled?: boolean; destructive?: boolean; onClick(): void }

export function RoutingActions({ label, actions, disabled }: { label: string; actions: RoutingAction[]; disabled?: boolean }) {
  const [open, setOpen] = React.useState(false)
  return <Popover open={open} onOpenChange={setOpen}>
    <PopoverTrigger disabled={disabled} render={<Button variant='ghost' size='icon-sm' className='routing-more' aria-label={label} title={label} />}>
      <RouteIcon name='more' size={16} />
    </PopoverTrigger>
    <PopoverContent align='end' sideOffset={4} className='routing-actions-menu'>
      {actions.map(action => <Button key={action.label} variant='ghost' disabled={action.disabled}
        className={cn('routing-action-item', action.destructive && 'is-destructive')} onClick={() => { setOpen(false); action.onClick() }}>
        {action.icon ? <RouteIcon name={action.icon} size={14} /> : null}{action.label}
      </Button>)}
    </PopoverContent>
  </Popover>
}

export function RoutingProviderMark({ id, label }: { id: string; label: string }) {
  return PROVIDER_ICONS[id] ? <img className='routing-provider-mark' src={PROVIDER_ICONS[id]} alt='' />
    : <span className='routing-provider-mark routing-letter'>{label.slice(0, 1).toUpperCase()}</span>
}

export function routingOrderKey(event: React.KeyboardEvent, move: (direction: number) => void) {
  if (event.altKey && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
    event.preventDefault()
    move(event.key === 'ArrowUp' ? -1 : 1)
  }
}

export function RoutingAccountRow({ id, account, rank, joined, state, stateText, note, locked, orderLocked, orderHint, first, last,
  picked, live = 0, dragging, drop, globalAccounts = false, sorting, now, settingsDisabled = false, onDrag, onMove, onToggle, onAccount }: {
  id: string; account?: AccountRecord; rank: number; joined: boolean; state: string; stateText?: string; note: string
  locked: boolean; orderLocked: boolean; orderHint?: string; first: boolean; last: boolean
  picked?: boolean; live?: number; dragging?: boolean; drop?: 'before' | 'after'; globalAccounts?: boolean
  sorting?: AccountSorting | null; now?: number; settingsDisabled?: boolean
  onDrag(event: React.PointerEvent): void; onMove(direction: number): void; onToggle(): void; onAccount?: () => void
}) {
  const name = account ? displayNameOf(account) : t('账号已移除')
  // 在途数 > 0 的账号整行标出：路由页要回答的是「此刻这一单在谁身上」，
  // 只高亮连线（提供商一级）时，用户还得展开卡片逐行找是哪一个账号。
  const isLive = joined && live > 0
  return <div data-route-sort={joined ? id : undefined} data-live={isLive ? String(live) : undefined}
    className={cn('routing-config-account', !joined && 'is-excluded', picked && 'is-picked', isLive && 'is-live',
      dragging && 'is-dragging', drop && `drop-${drop}`)}>
    <Button variant='ghost' size='icon-sm' className='routing-grip' disabled={orderLocked || !joined}
      title={orderHint || t('拖动或按 Alt + 方向键调整顺序')} aria-label={t('拖动调整账号顺序')}
      onPointerDown={onDrag} onKeyDown={event => routingOrderKey(event, onMove)}><RouteIcon name='gripVertical' size={14} /></Button>
    <span className='routing-account-rank'>{joined ? rank : '—'}</span>
    <span className='routing-account-avatar' aria-hidden='true'>{name.slice(0, 1).toUpperCase()}</span>
    <span className='routing-config-account-name'><b title={name}>{name}</b>{note ? <small title={note}>{note}</small> : null}
      {account && now != null ? <RoutingCreditExpiry id={id} provider={providerOf(account)} sorting={sorting} now={now} /> : null}
    </span>
    {isLive ? <Badge variant='brand' shape='tag' className='routing-account-live' title={t('在途请求 / 并发上限')}>{t('{n} 在途', { n: live })}</Badge> : null}
    <RouteStateBadge state={state}>{stateText}</RouteStateBadge>
    {globalAccounts ? <Switch checked={joined} disabled={locked || !account} onCheckedChange={onToggle}
      aria-label={t('启用账号 {account}', { account: name })} title={joined ? t('禁用账号') : t('启用账号')} />
      : <Checkbox checked={joined} disabled={locked || (!account && !joined)} onCheckedChange={onToggle}
        aria-label={t('{account} 参与本路由', { account: name })} title={joined ? t('移出本路由') : t('加入路由')} />}
    {onAccount ? <Button variant='ghost' size='icon-sm' className='routing-settings-button' disabled={settingsDisabled}
      title={t('账号设置')} aria-label={t('{account} · 账号设置', { account: name })} onClick={onAccount}><RouteIcon name='settings' size={14} /></Button> : null}
    <RoutingActions label={t('账号操作')} actions={[
      { label: t('提高优先级'), icon: 'arrowUp', disabled: orderLocked || !joined || first, onClick: () => onMove(-1) },
      { label: t('降低优先级'), icon: 'arrowDown', disabled: orderLocked || !joined || last, onClick: () => onMove(1) },
    ]} />
  </div>
}
