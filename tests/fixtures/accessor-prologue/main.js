// Main script of the accessor-prologue check: no accessor ran before it.
if (getCount !== 0) throw new Error('compile ran a user accessor: ' + getCount);
print('ok');
