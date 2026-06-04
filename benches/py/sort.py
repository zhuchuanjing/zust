import sys

def bubble_sort(items):
    n = len(items)
    for i in range(n):
        for j in range(n - i - 1):
            if items[j] > items[j + 1]:
                items[j], items[j + 1] = items[j + 1], items[j]

n = int(sys.argv[1])
items = []
for i in range(n):
    seed = i * 6364136223846793005 + 1
    items.append(seed)
bubble_sort(items)
print(sum(items))
