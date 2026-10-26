/** 极简 className 合并（足够本项目使用，不引入额外依赖） */
export function cn(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(' ')
}
