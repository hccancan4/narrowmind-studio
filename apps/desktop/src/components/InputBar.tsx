import { useState, type FormEvent, type KeyboardEvent } from "react";

type Props = {
  disabled?: boolean;
  placeholder?: string;
  onSubmit: (text: string) => void;
};

export function InputBar({ disabled, placeholder, onSubmit }: Props) {
  const [value, setValue] = useState("");

  function submit() {
    const trimmed = value.trim();
    if (!trimmed || disabled) return;
    onSubmit(trimmed);
    setValue("");
  }

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    submit();
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }

  return (
    <form className="input-bar" onSubmit={handleSubmit}>
      <textarea
        rows={2}
        value={value}
        disabled={disabled}
        placeholder={placeholder ?? "ask the agent (enter to send, shift+enter for newline)"}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={handleKeyDown}
      />
      <button type="submit" disabled={disabled || !value.trim()}>
        {disabled ? "running…" : "send"}
      </button>
    </form>
  );
}
