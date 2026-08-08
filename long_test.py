# 斐波那契数列前20项

def fibonacci(n):
    """计算斐波那契数列前n项"""
    fibs = []
    a, b = 0, 1
    for _ in range(n):
        fibs.append(a)
        a, b = b, a + b
    return fibs

if __name__ == "__main__":
    result = fibonacci(20)
    print(f"斐波那契数列前20项：")
    for i, num in enumerate(result, 1):
        print(f"  F({i}) = {num}")
