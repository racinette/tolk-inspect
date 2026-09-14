import "b"

fun alpha(): int {
    return beta();
}

fun recursive(value: int): int {
    if (value <= 0) {
        return 0;
    }
    return recursive(value - 1);
}

