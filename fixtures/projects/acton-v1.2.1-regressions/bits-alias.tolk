type bytes4 = bits32;

fun bytes4.isValid(self): bool {
    return true;
}

fun check(value: slice): bool {
    val digest = value as bytes4;
    return digest.isValid();
}
