/**
 * 云书 — 通信数据类型（移植自 demo/ui relay-types，适配本地 core 事件模型）
 */

export interface Device {
  id: string
  name: string
  ip: string
  online: boolean
}

export type MessageKind = 'text' | 'clipboard' | 'system'

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
