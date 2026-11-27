/* 跟系统有关的叫法：Windows 上是「回收站」「文件资源管理器」「这台电脑」，macOS 上是「废纸篓」「访达」「这台 Mac」 */

export const isWindows = (): boolean => navigator.userAgent.includes("Windows");

export const trashZh = (): string => (isWindows() ? "回收站" : "废纸篓");
export const trashEn = (): string => (isWindows() ? "Recycle Bin" : "Trash");
export const finderZh = (): string => (isWindows() ? "文件资源管理器" : "访达");
export const finderEn = (): string => (isWindows() ? "File Explorer" : "Finder");
export const thisMacZh = (): string => (isWindows() ? "这台电脑" : "这台 Mac");
export const thisMacEn = (): string => (isWindows() ? "this PC" : "this Mac");
export const yourMacEn = (): string => (isWindows() ? "your PC" : "your Mac");

/** 路径里的文件名（Windows 路径用反斜杠） */
export const baseName = (path: string): string => path.split(/[\\/]/).pop() ?? path;
