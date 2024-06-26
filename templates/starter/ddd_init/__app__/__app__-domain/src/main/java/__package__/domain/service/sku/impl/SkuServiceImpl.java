package __package__.domain.service.sku.impl;

import __package__.domain.model.sku.SkuDO;
import __package__.domain.repository.sku.SkuExampleRepository;
import __package__.domain.service.sku.SkuService;
import __package__.rpc.sku.SkuExampleRpc;
import __package__.rpc.sku.dto.SkuExampleDTO;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Service;

import java.util.List;

/**
 * SkuServiceImpl
 *
 * @author weirenyan
 * @date 2023/12/6
 */

@Service
public class SkuServiceImpl implements SkuService {

    /**
     * Sku Example Repository
     */
    @Autowired
    private SkuExampleRepository skuExampleRepository;

    /**
     * Sku Example Rpc
     */
    @Autowired
    private SkuExampleRpc skuExampleRpc;

    public SkuDO getSku(Long skuId) {
        SkuDO sku = skuExampleRepository.getSku(skuId);

        List<SkuExampleDTO> skuExampleDTOS = skuExampleRpc.fetchSkuList(List.of(String.valueOf(skuId)));

        if (skuExampleDTOS != null && !skuExampleDTOS.isEmpty()) {

            SkuExampleDTO exampleDTO =skuExampleDTOS.get(0);

            sku.setOriginAddress(exampleDTO.getOriginAddress());
            sku.setShortTitle(exampleDTO.getShortTitle());
        }

        return sku;
    }
}
