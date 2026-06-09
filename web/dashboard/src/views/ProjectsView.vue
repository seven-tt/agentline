<script setup lang="ts">
import { inject, ref, reactive, onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ProjectItem } from '../types'
import { api } from '../api'

const { t } = useI18n()
const addToast = inject<(type: 'success' | 'error' | 'info', msg: string) => void>('addToast')!

const projects = reactive<ProjectItem[]>([])

const editing = ref(false)
const editName = ref('')
const editUrl = ref('')
const editIndex = ref(-1)

function startAdd() {
  editIndex.value = -1
  editName.value = ''
  editUrl.value = ''
  editing.value = true
}

function startEdit(i: number) {
  const p = projects[i]
  editIndex.value = i
  editName.value = p.name
  editUrl.value = p.git_url
  editing.value = true
}

function cancelEdit() {
  editing.value = false
}

async function saveProject() {
  if (!editName.value.trim() || !editUrl.value.trim()) return
  const item: ProjectItem = { name: editName.value.trim(), git_url: editUrl.value.trim() }
  const list = [...projects]
  if (editIndex.value >= 0) {
    list[editIndex.value] = item
  } else {
    list.push(item)
  }
  try {
    await api.saveProjects(list)
    projects.splice(0, projects.length, ...list)
    editing.value = false
    addToast('success', t('projects.project_saved'))
  } catch (e: any) {
    addToast('error', e.message)
  }
}

async function removeProject(i: number) {
  const list = projects.filter((_, idx) => idx !== i)
  try {
    await api.saveProjects(list)
    projects.splice(0, projects.length, ...list)
    addToast('success', t('projects.project_deleted'))
  } catch (e: any) {
    addToast('error', e.message)
  }
}

onMounted(async () => {
  try {
    const data = await api.getProjects()
    projects.splice(0, projects.length, ...data)
  } catch { /* ignore */ }
})
</script>

<template>
  <div class="card">
    <div class="card-head">
      <h3>{{ $t('projects.project_list') }}</h3>
      <button class="btn btn-sm btn-ghost" @click="startAdd">{{ $t('projects.add_project') }}</button>
    </div>
    <div class="card-body">
      <div v-if="projects.length === 0 && !editing" class="empty-state">
        {{ $t('projects.empty_state') }}
      </div>
      <div v-for="(p, i) in projects" :key="i" class="project-row">
        <span class="project-name">{{ p.name }}</span>
        <span class="project-url">{{ p.git_url }}</span>
        <button class="btn btn-sm btn-ghost" @click="startEdit(i)">{{ $t('common.edit') }}</button>
        <button class="btn btn-sm btn-danger" @click="removeProject(i)">{{ $t('common.delete') }}</button>
      </div>
    </div>
  </div>

  <div v-if="editing" class="card mt-4">
    <div class="card-head">
      <h3>{{ editIndex >= 0 ? $t('projects.edit_project') : $t('projects.add_project_title') }}</h3>
    </div>
    <div class="card-body">
      <div class="grid grid-2">
        <div class="field">
          <label class="field-label">{{ $t('projects.project_name') }}</label>
          <input type="text" v-model="editName" placeholder="e.g. my-app" />
        </div>
        <div class="field">
          <label class="field-label">{{ $t('projects.git_url') }}</label>
          <input type="text" v-model="editUrl" class="input-mono" placeholder="https://github.com/org/repo.git" />
        </div>
      </div>
      <div class="flex-row-end mt-4">
        <button class="btn btn-primary" @click="saveProject">{{ $t('common.save') }}</button>
        <button class="btn btn-ghost" @click="cancelEdit">{{ $t('common.cancel') }}</button>
      </div>
    </div>
  </div>
</template>
