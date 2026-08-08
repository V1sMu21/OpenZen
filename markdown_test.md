# Comprehensive Markdown Test

## Language Comparison Table

| Feature | Python | Rust | Go |
|---|---|---|---|
| **Speed** | Slow (interpreted) | Very Fast (compiled, zero-cost abstractions) | Fast (compiled) |
| **Type Safety** | Dynamic typing, optional type hints | Strict static typing, ownership system | Static typing, garbage collected |
| **Learning Curve** | Easy — beginner-friendly syntax | Steep — ownership/borrowing concepts | Moderate — simple syntax, minimal boilerplate |

## Python Fibonacci Function

```python
def fibonacci(n: int) -> int:
    """Return the n-th Fibonacci number (0-indexed)."""
    if n < 0:
        raise ValueError("n must be non-negative")
    if n <= 1:
        return n
    a, b = 0, 1
    for _ in range(2, n + 1):
        a, b = b, a + b
    return b
```

## Three Notable Points

- Markdown supports **rich text formatting** including headers, lists, and tables.
- Math expressions can be written inline or as standalone blocks using standard delimiters.
- Code blocks with syntax highlighting improve readability and are widely supported across platforms.

## Math Expressions

Inline math: $E=mc^2$

Block math:
$$
x = \frac{-b \pm \sqrt{b^2-4ac}}{2a}
$$

## Formatting Test Paragraph

This paragraph demonstrates **bold text** for emphasis, *italic text* for subtle styling, and `inline code` snippets — all of which are fundamental Markdown features that render consistently across virtually every parser and platform in the ecosystem.
