import { invoke } from '@tauri-apps/api/core'
import { message, Modal } from 'ant-design-vue'
import { i18n } from '@/i18n'
import { useDirectoryStore } from '@/stores/directory'
import { useSettingsStore } from '@/stores/settings'
import { useViewerStore } from '@/stores/viewer'

export function useFileOperations() {
  const directoryStore = useDirectoryStore()
  const settingsStore = useSettingsStore()
  const viewerStore = useViewerStore()

  const withCurrentPath = async (action: (path: string) => Promise<void>) => {
    const path = directoryStore.currentEntry?.path
    if (path) await action(path)
  }

  const deleteCurrent = async () => {
    const perform = async () => withCurrentPath(async (path) => {
      await invoke('move_to_trash', { path })
      directoryStore.removeCurrent()
      void message.success(i18n.global.t('file.deleted'))
    })
    if (!settingsStore.settings.viewer.confirmDelete) return perform()
    Modal.confirm({
      title: i18n.global.t('file.confirmDeleteTitle'),
      content: i18n.global.t('file.confirmDeleteMessage'),
      okText: i18n.global.t('file.delete'),
      okType: 'danger',
      cancelText: i18n.global.t('action.cancel'),
      onOk: perform,
    })
  }

  return {
    rotateClockwise: () => viewerStore.rotate(true),
    rotateCounterClockwise: () => viewerStore.rotate(false),
    deleteCurrent,
    revealCurrent: () => withCurrentPath(path => invoke('reveal_in_finder', { path })),
    copyCurrentFile: () => withCurrentPath(async (path) => {
      await invoke('copy_file_to_clipboard', { path })
      void message.success(i18n.global.t('file.copied'))
    }),
    copyCurrentPath: () => withCurrentPath(async (path) => {
      await navigator.clipboard.writeText(path)
      void message.success(i18n.global.t('file.pathCopied'))
    }),
  }
}
