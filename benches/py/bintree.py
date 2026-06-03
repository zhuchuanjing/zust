import sys
def make_tree(depth):
    if depth <= 0: return 1
    return 1 + make_tree(depth - 1) + make_tree(depth - 1)
n = int(sys.argv[1])
print(make_tree(n))
