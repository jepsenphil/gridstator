#include <mach/mach.h>
#include <stdio.h>
int main(void){mach_port_t host=mach_host_self();vm_size_t size=0;vm_statistics64_data_t s={0};mach_msg_type_number_t count=sizeof(s)/sizeof(integer_t);kern_return_t p=host_page_size(host,&size);kern_return_t k=host_statistics64(host,HOST_VM_INFO64,(host_info64_t)&s,&count);printf("page_result=%d page_size=%lu statistics_result=%d count=%u expected=%u free=%u inactive=%u\n",p,(unsigned long)size,k,count,(unsigned)(sizeof(s)/sizeof(integer_t)),s.free_count,s.inactive_count);mach_port_deallocate(mach_task_self(),host);}
