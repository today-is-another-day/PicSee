<script setup lang="ts">
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
</script>

<template>
  <section class="image-viewer">
    <div class="image-viewer__placeholder">
      <div class="image-viewer__icon" aria-hidden="true">PIC</div>
      <h1>{{ t('placeholder.viewerTitle') }}</h1>
      <p>{{ t('placeholder.viewerDescription') }}</p>
    </div>
  </section>
</template>

<style scoped>
.image-viewer {
  display: grid;
  min-width: 0;
  min-height: 0;
  flex: 1;
  place-items: center;
  overflow: hidden;
  background-color: var(--canvas-bg);
  background-image: radial-gradient(circle at center, var(--canvas-glow), transparent 55%);
}

.image-viewer__placeholder {
  max-width: 420px;
  padding: 32px;
  text-align: center;
}

.image-viewer__icon {
  display: grid;
  width: 72px;
  height: 72px;
  margin: 0 auto;
  place-items: center;
  border: 2px solid currentColor;
  border-radius: 18px;
  color: #1677ff;
  font-size: 18px;
  font-weight: 700;
}

.image-viewer__placeholder h1 {
  margin: 18px 0 8px;
  font-size: 24px;
}

.image-viewer__placeholder p {
  margin: 0;
  color: var(--muted-color);
}
</style>
