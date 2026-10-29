/**
 * Yunshu — 通信数据类型（移植自 demo/ui relay-types，适配本地 core 事件模型）
 */

export interface Device {
  id: string
  name: string
  ip: string
  online: boolean
  /** 公开机器 ID（静态公钥指纹前 8 位 hex）；远程控制以此定位设备，旧版本为空 */
  machineId?: string
  /** 手动添加的主机（自助添加，不经广播发现） */
  manual?: boolean
  /** 可达地址 host:port（手动主机的移除/重连用） */
  addr?: string
}

export type MessageKind = 'text' | 'system'

/** 头像右下角状态灯：online 绿 / stealth 隐身黄 / dnd 免打扰紫 / offline 灰 */
export type Presence = 'online' | 'stealth' | 'dnd' | 'offline'

export interface ChatMessage {
  id: string
  from: string // deviceId 或 'me'
  fromName: string
  to: string // deviceId
  kind: MessageKind
  content: string
  createdAt: number
}

export interface TransferFileMeta {
  id: string
  name: string
  size: number
}

export type TransferStatus =
  | 'pending'
  | 'rejected'
  | 'uploading'
  | 'paused'
  | 'completed'
  | 'cancelled'

export interface Transfer {
  id: string
  from: string // 'me' 或 deviceId
  fromName: string
  to: string
  toName: string
  kind: 'files' | 'folder'
  files: TransferFileMeta[]
  totalSize: number
  status: TransferStatus
  sentBytes: number
  createdAt: number
  /** 完成后本地路径（打开所在文件夹） */
  path?: string
}

export type ConnectionState = 'connected' | 'connecting' | 'disconnected'

/** 聊天流渲染条目：消息与传输卡片合并 */
export type ChatEntry =
  | { type: 'message'; key: string; message: ChatMessage }
  | { type: 'transfer'; key: string; transfer: Transfer }

export type FileCategory =
  | 'image' | 'video' | 'audio' | 'archive' | 'pdf' | 'doc' | 'code' | 'other'
