<script lang="ts">
  // A labeled numeric input. type="number" is static so Svelte's bind:value
  // coerces to a number (a dynamic type attribute would not). Self-styled
  // because scoped parent rules can't reach a child component's input.
  let {
    label,
    value = $bindable(),
    step,
    min,
    max,
  }: {
    label: string;
    value: number;
    step?: number;
    min?: number;
    max?: number;
  } = $props();
</script>

<label class="field">
  {label}
  <input type="number" {step} {min} {max} bind:value />
</label>

<style>
  .field {
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .field input {
    width: 5em;
    font-size: 11px;
    padding: 1px 3px;
  }
</style>
